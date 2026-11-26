//! Telling a person in their browser that something waits for them, while no
//! Tilo of theirs is open: Web Push (RFC 8030), its message encrypted for the
//! browser that subscribed (RFC 8291, `aes128gcm` of RFC 8188) and the request
//! signed with this host's application server key (VAPID, RFC 8292).
//!
//! A browser of the person's that may show notifications is kept here
//! ([`WebPush::subscribe`]); this host keeps a few per person on disk. When the teller has something for a person
//! and no device of theirs is looking, every browser they subscribed is
//! pushed a short message. Its service worker shows it, and a click opens
//! Tilo, where it waits.
//!
//! The same sender reaches an app of the person's that is closed, where its
//! system takes web push instead of the system's own service: a phone
//! whose `UnifiedPush` distributor gave it an endpoint hands that endpoint to
//! this host with a key of its own (`workspaces::push_addresses`), and is
//! pushed the title, the body and the buttons of what it is told, encrypted
//! for that key alone. Nothing in that push says where the thing is, nor a
//! workspace or an id: the endpoint, the push service and anything between
//! see only an encrypted body.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::KeyPair as _;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

#[cfg(feature = "agents")]
use tilo_actions::remote::person_telling::Told;
#[cfg(feature = "agents")]
use tilo_person_services::push_addresses::PushKind;

/// The file this host's application server key is kept in, under its state.
const KEY_FILE: &str = "web-push-key.pk8";
/// The file the subscriptions are kept in, under its state.
const SUBSCRIPTIONS_FILE: &str = "web-push-subscriptions.json";
/// The most browsers one person is pushed to: the newest are kept.
const PER_PERSON: usize = 8;
/// The most people kept.
const PEOPLE: usize = 100_000;
/// How long a push service keeps a message for a browser that is offline.
const TTL_SECS: u64 = 10 * 60;
/// The longest a message may be, before encryption: one record.
const MAX_MESSAGE: usize = 3_000;
/// The record size the message is sent with: one record holds it all.
const RECORD_SIZE: u32 = 4096;

/// This server's push sender: its key and the subscriptions people gave it.
/// A clone is the same sender.
#[derive(Clone)]
pub struct WebPush {
    key: std::sync::Arc<ring::signature::EcdsaKeyPair>,
    /// Who the push services may write to about this host (`mailto:` or a
    /// URL), as VAPID's `sub` claim names it.
    contact: String,
    path: PathBuf,
    kept: std::sync::Arc<Mutex<Kept>>,
    client: tilo_net::Client,
    /// Where a person's closed apps are reached: their devices' web push
    /// addresses, which this host keeps. None where this sender pushes to
    /// browsers only.
    #[cfg(feature = "agents")]
    workspaces: Option<crate::workspaces::Workspaces>,
    /// Whether it may reach a push service on this machine: tests do, and
    /// nothing else.
    here: bool,
}

/// Where a browser takes pushes, as its `PushManager` subscribed
/// (RFC 8030, RFC 8291): its push service's address for it, its key and its
/// auth secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSubscription {
    /// The push service's `https` address for this browser.
    pub endpoint: String,
    /// The browser's P-256 key, uncompressed (65 bytes).
    pub p256dh: Vec<u8>,
    /// The browser's auth secret (16 bytes).
    pub auth: Vec<u8>,
}

impl PushSubscription {
    /// The longest push service address taken.
    pub const MAX_ENDPOINT: usize = 1024;

    /// Whether this is a subscription a host can push to: an `https`
    /// address of bounded length, a key and an auth secret of their sizes.
    ///
    /// # Errors
    /// What is wrong with it.
    pub fn check(&self) -> Result<(), &'static str> {
        if !self.endpoint.starts_with("https://") || self.endpoint.len() > Self::MAX_ENDPOINT {
            return Err("a push subscription names an https address");
        }
        if self.p256dh.len() != 65 || self.p256dh.first() != Some(&4) {
            return Err("a push subscription carries the browser's P-256 key");
        }
        if self.auth.len() != 16 {
            return Err("a push subscription carries a 16-byte auth secret");
        }
        Ok(())
    }
}

/// The subscriptions kept, by person (their root, hex).
#[derive(Default, Serialize, Deserialize)]
struct Kept {
    people: BTreeMap<String, Vec<PushSubscription>>,
}

/// What a browser's service worker shows.
#[derive(Debug, Serialize)]
pub struct Message<'a> {
    pub title: &'a str,
    pub body: &'a str,
    /// Replaces an earlier notification with the same tag.
    pub tag: &'a str,
    /// What a click opens.
    pub url: &'a str,
}

impl WebPush {
    /// The sender kept under `state`: its key made the first time, the
    /// subscriptions read back. `contact` is the VAPID subject.
    ///
    /// # Errors
    ///
    /// The key or subscriptions could not be read or made.
    pub fn open(state: &Path, contact: &str) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let key_path = state.join(KEY_FILE);
        let pkcs8 = match std::fs::read(&key_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let made = ring::signature::EcdsaKeyPair::generate_pkcs8(
                    &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                    &SystemRandom::new(),
                )
                .map_err(|_| anyhow::anyhow!("no randomness for the push key"))?;
                tilo_files::native::private::write_atomic(&key_path, made.as_ref())
                    .with_context(|| format!("write {}", key_path.display()))?;
                made.as_ref().to_vec()
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", key_path.display()));
            }
        };
        let key = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|_| anyhow::anyhow!("{} is no push key", key_path.display()))?;
        let path = state.join(SUBSCRIPTIONS_FILE);
        let kept = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Kept::default(),
        };
        let client = client()?;
        Ok(Self {
            key: std::sync::Arc::new(key),
            contact: contact.to_owned(),
            path,
            kept: std::sync::Arc::new(Mutex::new(kept)),
            client,
            #[cfg(feature = "agents")]
            workspaces: None,
            here: false,
        })
    }

    /// This sender, reaching the closed apps of `workspaces`' people too: a
    /// device of theirs that handed over a web push address is pushed what
    /// it is told, through the same key, the same encryption and the same
    /// client as a browser is.
    #[cfg(feature = "agents")]
    #[must_use]
    pub fn telling(mut self, workspaces: crate::workspaces::Workspaces) -> Self {
        self.workspaces = Some(workspaces);
        self
    }

    /// A sender that may post to a push service on this machine, which only a test stands up:
    /// nothing else reaches an address of its own network. `trusted` is the DER of the certificate
    /// that push service answers with, the only one trusted.
    ///
    /// # Errors
    ///
    /// The key or subscriptions could not be read or made, or the certificate is not one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn reaching_here(state: &Path, contact: &str, trusted: &[u8]) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(trusted.to_vec()))
            .context("the test push service's certificate")?;
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut push = Self::open(state, contact)?;
        push.client = tilo_net::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirects(tilo_net::Redirects::None)
            .direct()
            .tls(tls)
            .build()
            .context("the test push client")?;
        push.here = true;
        Ok(push)
    }

    /// This host's application server key, as a browser subscribes with it:
    /// the uncompressed P-256 point.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.key.public_key().as_ref().to_vec()
    }

    /// Keep `subscription` as one of `person`'s browsers, the newest first.
    ///
    /// # Errors
    ///
    /// The subscription is malformed, or could not be kept.
    pub fn subscribe(
        &self,
        person: &[u8; 32],
        subscription: PushSubscription,
    ) -> Result<(), String> {
        subscription.check()?;
        public_endpoint(&subscription.endpoint)?;
        let mut kept = self.kept.lock_recover();
        let key = hex::encode(person);
        if !kept.people.contains_key(&key) && kept.people.len() >= PEOPLE {
            return Err("this host keeps no more browsers to notify".into());
        }
        let browsers = kept.people.entry(key).or_default();
        browsers.retain(|known| known.endpoint != subscription.endpoint);
        browsers.insert(0, subscription);
        browsers.truncate(PER_PERSON);
        let saved = self.save(&kept);
        drop(kept);
        saved
    }

    fn save(&self, kept: &Kept) -> Result<(), String> {
        let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
        tilo_files::native::private::write_atomic(&self.path, &bytes)
            .map_err(|error| error.to_string())
    }

    /// Push `message` to every browser `person` subscribed. A browser whose
    /// push service says it is gone is forgotten. Answers how many took it.
    pub async fn notify(&self, person: &[u8; 32], message: &Message<'_>) -> usize {
        let browsers = self
            .kept
            .lock_recover()
            .people
            .get(&hex::encode(person))
            .cloned()
            .unwrap_or_default();
        let Ok(mut plain) = serde_json::to_vec(message) else {
            return 0;
        };
        plain.truncate(MAX_MESSAGE);
        let mut delivered: usize = 0;
        let mut gone = Vec::new();
        for browser in browsers {
            match self.push(&browser, &plain).await {
                Ok(()) => delivered = delivered.saturating_add(1),
                Err(Pushed::Gone) => gone.push(browser.endpoint),
                Err(Pushed::Failed(reason)) => {
                    tracing::debug!(%reason, "a push to a person's browser failed");
                }
            }
        }
        if !gone.is_empty() {
            let mut kept = self.kept.lock_recover();
            if let Some(browsers) = kept.people.get_mut(&hex::encode(person)) {
                browsers.retain(|browser| !gone.contains(&browser.endpoint));
            }
            let _ = self.save(&kept);
            drop(kept);
        }
        delivered
    }

    async fn push(&self, browser: &PushSubscription, plain: &[u8]) -> Result<(), Pushed> {
        self.post(&browser.endpoint, browser, plain).await
    }

    /// Post `plain`, encrypted for the key and auth secret of the device at
    /// `endpoint`, to that endpoint, signed as this host.
    async fn post(
        &self,
        endpoint: &str,
        key: &PushSubscription,
        plain: &[u8],
    ) -> Result<(), Pushed> {
        if !self.here && public_endpoint(endpoint).is_err() {
            // Kept before an address was judged: never posted to, forgotten.
            return Err(Pushed::Gone);
        }
        let body = encrypt(key, plain).map_err(|()| Pushed::Failed("encrypt".into()))?;
        let authorization = self
            .vapid(endpoint)
            .ok_or_else(|| Pushed::Failed("sign".into()))?;
        let response = self
            .client
            .post(endpoint)
            .header("authorization", authorization)
            .header("content-encoding", "aes128gcm")
            .header("content-type", "application/octet-stream")
            .header("ttl", TTL_SECS.to_string())
            .header("urgency", "high")
            .body(body)
            .send()
            .await
            .map_err(|error| Pushed::Failed(error.to_string()))?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            404 | 410 => Err(Pushed::Gone),
            status => Err(Pushed::Failed(format!(
                "the push service answered {status}"
            ))),
        }
    }

    /// The `Authorization` header for a push to `endpoint`: a JWT for its
    /// origin this host's key signs, and that key.
    fn vapid(&self, endpoint: &str) -> Option<String> {
        let url = tilo_net::Url::parse(endpoint).ok()?;
        let audience = url.origin().ascii_serialization();
        let expires = (tilo_clock::unix_now_ns() / 1_000_000_000).saturating_add(12 * 60 * 60);
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::json!({"aud": audience, "exp": expires, "sub": self.contact}).to_string(),
        );
        let signed = format!("{header}.{claims}");
        let signature = self
            .key
            .sign(&SystemRandom::new(), signed.as_bytes())
            .ok()?;
        Some(format!(
            "vapid t={signed}.{}, k={}",
            URL_SAFE_NO_PAD.encode(signature.as_ref()),
            URL_SAFE_NO_PAD.encode(self.public_key())
        ))
    }
}

/// The client a push is posted with. A browser names the address: only the
/// public internet is dialled, judged at the connection's own lookup, and
/// no proxy of this server's environment is taken, which would resolve the
/// name behind that lookup. A test's sender builds its own
/// (`WebPush::reaching_here`).
fn client() -> anyhow::Result<tilo_net::Client> {
    use anyhow::Context as _;
    tilo_net::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirects(tilo_net::Redirects::None)
        .direct()
        .lookup(tilo_net::address::public_only(
            tilo_net::address::system_lookup(),
        ))
        .build()
        .context("the push client")
}

/// Whether `endpoint` is an address this server posts a push to: one of the
/// web, and no address of this machine or its network written into it. A
/// name is judged where the connection looks it up.
fn public_endpoint(endpoint: &str) -> Result<(), &'static str> {
    let refusal = "a push subscription names a push service on the public internet";
    let url = tilo_net::Url::parse(endpoint).map_err(|_| refusal)?;
    match tilo_net::address::written_out(&url) {
        Some(address) if !tilo_net::address::is_public(address) => Err(refusal),
        _ => Ok(()),
    }
}

enum Pushed {
    /// The browser's subscription is gone: forget it.
    Gone,
    Failed(String),
}

/// `plain` encrypted for `browser` as one `aes128gcm` record (RFC 8291 §3.4,
/// RFC 8188 §2): a fresh key of this host's for this one message, agreed
/// with the browser's, keyed with the browser's auth secret.
pub(crate) fn encrypt(browser: &PushSubscription, plain: &[u8]) -> Result<Vec<u8>, ()> {
    use ring::{aead, agreement, hkdf, rand::SecureRandom as _};
    let random = SystemRandom::new();
    let ephemeral =
        agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &random).map_err(drop)?;
    let sender = ephemeral.compute_public_key().map_err(drop)?;
    let sender = sender.as_ref().to_vec();
    let receiver = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &browser.p256dh);
    let shared = agreement::agree_ephemeral(ephemeral, &receiver, <[u8]>::to_vec).map_err(drop)?;
    // IKM = HKDF(auth, ecdh, "WebPush: info" || 0 || ua_public || as_public, 32)
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(&browser.p256dh);
    key_info.extend_from_slice(&sender);
    let ikm = expand(
        &hkdf::Salt::new(hkdf::HKDF_SHA256, &browser.auth).extract(&shared),
        &key_info,
        32,
    )?;
    let mut salt = [0u8; 16];
    random.fill(&mut salt).map_err(drop)?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt).extract(&ikm);
    let cek = expand(&prk, b"Content-Encoding: aes128gcm\0", 16)?;
    let nonce = expand(&prk, b"Content-Encoding: nonce\0", 12)?;
    let key =
        aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(drop)?);
    // One record, the last: the content, then the delimiter 2.
    let mut record = plain.to_vec();
    record.push(2);
    key.seal_in_place_append_tag(
        aead::Nonce::try_assume_unique_for_key(&nonce).map_err(drop)?,
        aead::Aad::empty(),
        &mut record,
    )
    .map_err(drop)?;
    // Header: salt, record size, the sender's key's length and the key. The
    // one record ([`MAX_MESSAGE`] at most, its delimiter and tag) fits the
    // size every push service takes.
    let mut body = Vec::with_capacity(record.len().saturating_add(86));
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(u8::try_from(sender.len()).map_err(drop)?);
    body.extend_from_slice(&sender);
    body.extend_from_slice(&record);
    Ok(body)
}

/// `plain` encrypted for the device whose P-256 key and auth secret these are, as the sender
/// encrypts a push: what a receiver's test opens, without a push service between.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn encrypt_for_device(p256dh: &[u8], auth: &[u8], plain: &[u8]) -> Vec<u8> {
    encrypt(
        &PushSubscription {
            endpoint: String::new(),
            p256dh: p256dh.to_vec(),
            auth: auth.to_vec(),
        },
        plain,
    )
    .unwrap_or_default()
}

fn expand(prk: &ring::hkdf::Prk, info: &[u8], len: usize) -> Result<Vec<u8>, ()> {
    struct Len(usize);
    impl ring::hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let infos = [info];
    let okm = prk.expand(&infos, Len(len)).map_err(drop)?;
    let mut out = vec![0u8; len];
    okm.fill(&mut out).map_err(drop)?;
    Ok(out)
}

/// One thing told, as the app of a person's that is closed shows it: its
/// title, its body and its buttons, and the name only that device knows it
/// by, which a raised telling and a telling taken back are shown under. It
/// is encrypted for the device, so the push service reads none of it; and
/// it holds no id, no workspace, no route and no button target or
/// arguments, so nothing of where the thing is leaves the host.
#[cfg(feature = "agents")]
#[derive(Serialize)]
struct ToldPush<'a> {
    /// [`TOLD_PUSH_KIND`]: a thing waits.
    kind: &'static str,
    /// The name this device knows the thing by
    /// ([`tilo_actions::remote::person_telling::known_as`]).
    tag: &'a str,
    title: &'a str,
    body: &'a str,
    important: bool,
    /// How often it was raised, one the first time.
    raised: u32,
    actions: Vec<ToldButton<'a>>,
}

/// One button of a thing, as the app shows it: never what it invokes.
#[cfg(feature = "agents")]
#[derive(Serialize)]
struct ToldButton<'a> {
    id: &'a str,
    label: &'a str,
    main: bool,
}

/// A thing a push showed waits no more: a push that shows nothing, naming
/// the thing as this device knows it.
#[cfg(feature = "agents")]
#[derive(Serialize)]
struct ToldGone<'a> {
    kind: &'static str,
    tag: &'a str,
    gone: bool,
}

/// What a push says a thing is, so the app tells a telling from a telling
/// taken back.
#[cfg(feature = "agents")]
const TOLD_PUSH_KIND: &str = "told";

#[cfg(feature = "agents")]
impl WebPush {
    /// Push the thing `told`, as `title` and `body` say it, to every closed
    /// app of `person`'s that handed over a web push address. An address
    /// its push service says is gone is forgotten. Answers how many took
    /// it.
    async fn tell_apps(&self, person: &[u8; 32], told: &Told, title: &str, body: &str) -> usize {
        let Some(workspaces) = &self.workspaces else {
            return 0;
        };
        let mut took: Vec<String> = Vec::new();
        for address in tilo_person_services::PersonServices::of(&workspaces)
            .push_addresses(person)
            .into_iter()
            .filter(|address| address.kind == PushKind::WebPush)
        {
            let Ok((p256dh, auth)) = address.subscription_key() else {
                continue;
            };
            let tag = tilo_actions::remote::person_telling::known_as(&auth, &told.id);
            let said = |actions: Vec<ToldButton<'_>>, body: &str| {
                serde_json::to_vec(&ToldPush {
                    kind: TOLD_PUSH_KIND,
                    tag: &tag,
                    title,
                    body,
                    important: told.mark.important,
                    raised: told.insists.map_or(1, |insists| insists.raised),
                    actions,
                })
                .ok()
            };
            let buttons = || {
                told.actions
                    .iter()
                    .map(|action| ToldButton {
                        id: &action.id,
                        label: &action.label,
                        main: action.main,
                    })
                    .collect::<Vec<_>>()
            };
            let Some(plain) = said(buttons(), body)
                .filter(|plain| plain.len() <= MAX_MESSAGE)
                .or_else(|| {
                    // Too long with its buttons and its words: the app offers to open it.
                    said(Vec::new(), "Open Tilo to see it.")
                        .filter(|plain| plain.len() <= MAX_MESSAGE)
                })
            else {
                continue;
            };
            let key = PushSubscription {
                endpoint: address.endpoint.clone(),
                p256dh,
                auth,
            };
            match self.post(&address.endpoint, &key, &plain).await {
                Ok(()) => took.push(address.reached_as().to_owned()),
                Err(Pushed::Gone) => {
                    tilo_person_services::PersonServices::of(&workspaces).forget_push_address(
                        person,
                        address.reached_as(),
                        None,
                    );
                }
                Err(Pushed::Failed(reason)) => {
                    tracing::debug!(%reason, "a push to a person's closed app failed");
                }
            }
        }
        tilo_person_services::PersonServices::of(&workspaces).push_shown(person, &took, &told.id);
        took.len()
    }

    /// The things `ids` wait for `person` no more: every closed app a push
    /// showed one on is pushed a message that shows nothing and names the
    /// thing as gone, once.
    async fn gone_apps(&self, person: &[u8; 32], ids: &[String]) {
        let Some(workspaces) = &self.workspaces else {
            return;
        };
        for (address, shown) in tilo_person_services::PersonServices::of(&workspaces)
            .push_taken_back(person, ids, PushKind::WebPush)
        {
            let Ok((p256dh, auth)) = address.subscription_key() else {
                continue;
            };
            let key = PushSubscription {
                endpoint: address.endpoint.clone(),
                p256dh,
                auth: auth.clone(),
            };
            for id in shown {
                let tag = tilo_actions::remote::person_telling::known_as(&auth, &id);
                let Ok(plain) = serde_json::to_vec(&ToldGone {
                    kind: TOLD_PUSH_KIND,
                    tag: &tag,
                    gone: true,
                }) else {
                    continue;
                };
                if matches!(
                    self.post(&address.endpoint, &key, &plain).await,
                    Err(Pushed::Gone)
                ) {
                    tilo_person_services::PersonServices::of(&workspaces).forget_push_address(
                        person,
                        address.reached_as(),
                        None,
                    );
                    break;
                }
            }
        }
    }
}

/// What a person's assistant told them, pushed to the browsers they
/// subscribed and the closed apps of theirs that take web push. A push
/// message is encrypted for the device it goes to, so the push service
/// between reads none of it.
#[cfg(feature = "agents")]
impl tilo_app_host::agents::telling::Outside for WebPush {
    fn name(&self) -> &'static str {
        "web-push"
    }

    fn tell(
        &self,
        person: [u8; 32],
        told: tilo_actions::remote::person_telling::Told,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = u32> + Send>> {
        let push = self.clone();
        Box::pin(async move {
            let (title, body) = tilo_app_host::agents::telling::words_outside(&told);
            // One notification a thing: telling it again takes its place.
            let tag = format!("tilo-told-{}", told.id);
            let browsers = push
                .notify(
                    &person,
                    &Message {
                        title: &title,
                        body: &body,
                        tag: &tag,
                        url: "/",
                    },
                )
                .await;
            let apps = push.tell_apps(&person, &told, &title, &body).await;
            u32::try_from(browsers.saturating_add(apps)).unwrap_or(u32::MAX)
        })
    }

    fn taken_back(
        &self,
        person: [u8; 32],
        ids: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let push = self.clone();
        Box::pin(async move { push.gone_apps(&person, &ids).await })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::{aead, agreement, hkdf};

    /// What a browser does with a push: agree on the key with its own
    /// private key and auth secret, and open the one record.
    pub(crate) fn decrypt(
        own: agreement::EphemeralPrivateKey,
        own_public: &[u8],
        auth: &[u8],
        body: &[u8],
    ) -> Vec<u8> {
        let salt = &body[..16];
        let id_len = usize::from(body[20]);
        let sender = &body[21..21 + id_len];
        let record = &body[21 + id_len..];
        let shared = agreement::agree_ephemeral(
            own,
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, sender),
            <[u8]>::to_vec,
        )
        .unwrap();
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(own_public);
        info.extend_from_slice(sender);
        let ikm = expand(
            &hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(&shared),
            &info,
            32,
        )
        .unwrap();
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
        let cek = expand(&prk, b"Content-Encoding: aes128gcm\0", 16).unwrap();
        let nonce = expand(&prk, b"Content-Encoding: nonce\0", 12).unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut record = record.to_vec();
        let opened = key
            .open_in_place(
                aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
                aead::Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(opened.last(), Some(&2), "one record, the last");
        opened[..opened.len().saturating_sub(1)].to_vec()
    }

    #[test]
    fn a_browser_opens_what_it_is_pushed_and_nothing_else_does() {
        let random = SystemRandom::new();
        let browser =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &random).unwrap();
        let public = browser.compute_public_key().unwrap().as_ref().to_vec();
        let subscription = PushSubscription {
            endpoint: "https://push.example/send/abc".into(),
            p256dh: public.clone(),
            auth: vec![7; 16],
        };
        let body = encrypt(&subscription, br#"{"title":"Visa backend needs you"}"#).unwrap();
        assert_eq!(
            decrypt(browser, &public, &[7; 16], &body),
            br#"{"title":"Visa backend needs you"}"#
        );
    }

    #[test]
    fn the_vapid_header_names_the_push_service_and_this_hosts_key() {
        let state = tempfile::tempdir().unwrap();
        let push = WebPush::open(state.path(), "mailto:ops@example.com").unwrap();
        let header = push.vapid("https://push.example/send/abc").unwrap();
        let (token, key) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(key).unwrap(), push.public_key());
        let claims = token.split('.').nth(1).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example");
        assert_eq!(claims["sub"], "mailto:ops@example.com");
        // The key is kept: a host that restarts signs with the same one,
        // so the browsers subscribed to it keep working.
        let again = WebPush::open(state.path(), "mailto:ops@example.com").unwrap();
        assert_eq!(again.public_key(), push.public_key());
    }

    const ADA: [u8; 32] = [0xad; 32];

    /// A subscription a push can be encrypted for, at `endpoint`.
    fn subscription(endpoint: &str) -> PushSubscription {
        let browser =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &SystemRandom::new())
                .unwrap();
        PushSubscription {
            endpoint: endpoint.into(),
            p256dh: browser.compute_public_key().unwrap().as_ref().to_vec(),
            auth: vec![7; 16],
        }
    }

    fn a_question() -> Message<'static> {
        Message {
            title: "An agent needs you",
            body: "Open Tilo to answer.",
            tag: "tilo-question-1",
            url: "/",
        }
    }

    /// A listener on this machine that counts the connections made to it and
    /// closes each at once, so whoever connected is not kept waiting.
    struct Here {
        port: u16,
        connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Here {
        async fn listen() -> Self {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = std::sync::Arc::clone(&connections);
            tokio::spawn(async move {
                while let Ok((connection, _)) = listener.accept().await {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    drop(connection);
                }
            });
            Self { port, connections }
        }

        fn connections(&self) -> usize {
            self.connections.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// What is kept on disk under `state`, by person.
    fn kept_under(state: &Path) -> Kept {
        std::fs::read(state.join(SUBSCRIPTIONS_FILE))
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or_default()
    }

    /// A browser's subscription names the address this server posts to. One
    /// that names this machine or its network, in any form an address takes,
    /// is not kept; a push service on the public internet is.
    #[test]
    fn a_subscription_that_names_an_own_address_is_not_kept() {
        let state = tempfile::tempdir().unwrap();
        let push = WebPush::open(state.path(), "mailto:ops@example.com").unwrap();
        for own in [
            "https://127.0.0.1/send/abc",
            "https://127.0.0.1:8443/send/abc",
            "https://10.0.0.7/send/abc",
            "https://192.168.1.1/send/abc",
            "https://100.64.0.1/send/abc",
            "https://169.254.169.254/latest/meta-data",
            "https://168.63.129.16/machine",
            "https://0.0.0.0/send/abc",
            "https://[::1]/send/abc",
            "https://[::ffff:127.0.0.1]/send/abc",
            "https://[64:ff9b::a00:7]/send/abc",
            "https://[2002:a00:7::]/send/abc",
            "https://[fd00::7]/send/abc",
            "https://[fe80::1]/send/abc",
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            assert!(
                push.subscribe(&ADA, subscription(own)).is_err(),
                "{own} names this machine or its network and was kept"
            );
            assert!(
                public_endpoint(own).is_err(),
                "{own} must be refused by public_endpoint"
            );
        }
        assert!(
            kept_under(state.path()).people.is_empty(),
            "nothing of them is kept"
        );
        push.subscribe(&ADA, subscription("https://push.example/send/abc"))
            .unwrap();
        assert_eq!(kept_under(state.path()).people.len(), 1);
    }

    /// A name is judged where the connection looks it up: a push to a name
    /// that answers with an address of this machine connects to nothing.
    #[tokio::test]
    async fn a_push_to_a_name_that_answers_with_this_machine_connects_to_nothing() {
        let here = Here::listen().await;
        let state = tempfile::tempdir().unwrap();
        let push = WebPush::open(state.path(), "mailto:ops@example.com").unwrap();
        push.subscribe(
            &ADA,
            subscription(&format!("https://localhost:{}/send/abc", here.port)),
        )
        .unwrap();

        assert_eq!(push.notify(&ADA, &a_question()).await, 0);
        assert_eq!(
            here.connections(),
            0,
            "a push to a name that answers with this machine connected to it"
        );
    }

    /// An address of this machine that was kept before subscriptions were
    /// judged is never posted to, and is forgotten.
    #[tokio::test]
    async fn a_push_to_an_own_address_kept_earlier_connects_to_nothing_and_forgets_it() {
        let here = Here::listen().await;
        let state = tempfile::tempdir().unwrap();
        let mut kept = Kept::default();
        kept.people.insert(
            hex::encode(ADA),
            vec![subscription(&format!(
                "https://127.0.0.1:{}/send/abc",
                here.port
            ))],
        );
        // Kept as the server keeps it: a private file.
        tilo_files::native::private::write_atomic(
            &state.path().join(SUBSCRIPTIONS_FILE),
            &serde_json::to_vec(&kept).unwrap(),
        )
        .unwrap();
        let push = WebPush::open(state.path(), "mailto:ops@example.com").unwrap();

        assert_eq!(push.notify(&ADA, &a_question()).await, 0);
        assert_eq!(
            here.connections(),
            0,
            "a push to an address of this machine kept earlier connected to it"
        );
        assert!(
            kept_under(state.path()).people.values().all(Vec::is_empty),
            "an address of this machine is forgotten, not tried again"
        );
    }

    /// A push takes no proxy from this server's environment: such a proxy
    /// would resolve the name itself, behind the lookup a push is judged by,
    /// and so reach what that lookup refuses.
    #[tokio::test]
    async fn a_push_sends_nothing_to_a_proxy_the_environment_names() {
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "a_push_sends_nothing_to_a_proxy_the_environment_names",
        ) {
            return;
        }
        let proxy = Here::listen().await;
        let here = Here::listen().await;
        let mut environment = tilo_test_env::ScopedEnv::new();
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            environment.set(name, format!("http://127.0.0.1:{}", proxy.port));
        }
        // Nothing exempts an address from that proxy, and nothing has a name of it ignored.
        for name in ["NO_PROXY", "no_proxy", "REQUEST_METHOD"] {
            environment.remove(name);
        }
        let state = tempfile::tempdir().unwrap();
        let push = WebPush::open(state.path(), "mailto:ops@example.com").unwrap();
        push.subscribe(
            &ADA,
            subscription(&format!("https://localhost:{}/send/abc", here.port)),
        )
        .unwrap();

        assert_eq!(push.notify(&ADA, &a_question()).await, 0);
        assert_eq!(
            proxy.connections(),
            0,
            "the proxy this server's environment names was sent a push's connection"
        );
        assert_eq!(
            here.connections(),
            0,
            "a push to a name that answers with this machine connected to it"
        );
    }
}
