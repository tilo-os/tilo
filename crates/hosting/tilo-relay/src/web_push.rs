//! Telling a person in their browser that an agent waits for them, while no
//! Tilo of theirs is open: Web Push (RFC 8030), its message encrypted for the
//! browser that subscribed (RFC 8291, `aes128gcm` of RFC 8188) and the request
//! signed with this host's application server key (VAPID, RFC 8292).
//!
//! A page of the person's that may show notifications subscribes its browser
//! and hands the subscription here ([`ApproverRequest::Notify`]); this host
//! keeps a few per person on disk. When a question waits for a person and no
//! device of theirs has looked for questions lately, every browser they
//! subscribed is pushed a short message: who waits, and for what. Its service
//! worker shows it, and a click opens Tilo, where the question waits.
//!
//! The message names the agent and the action, never the page's contents or
//! picture: what the person decides on is shown in Tilo, where they answer.
//!
//! [`ApproverRequest::Notify`]: tilo_relay_protocol::certify::ApproverRequest::Notify

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
use tilo_relay_protocol::certify::PushSubscription;

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

/// This host's push sender: its key and the subscriptions people gave it.
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
        let client = tilo_net::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirects(tilo_net::Redirects::None)
            .build()
            .context("the push client")?;
        Ok(Self {
            key: std::sync::Arc::new(key),
            contact: contact.to_owned(),
            path,
            kept: std::sync::Arc::new(Mutex::new(kept)),
            client,
        })
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
        let body = encrypt(browser, plain).map_err(|()| Pushed::Failed("encrypt".into()))?;
        let authorization = self
            .vapid(&browser.endpoint)
            .ok_or_else(|| Pushed::Failed("sign".into()))?;
        let response = self
            .client
            .post(&browser.endpoint)
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

impl crate::certification::QuestionNotifier for WebPush {
    fn push_key(&self) -> Vec<u8> {
        self.public_key()
    }

    fn subscribe(&self, person: &[u8; 32], subscription: PushSubscription) -> Result<(), String> {
        Self::subscribe(self, person, subscription)
    }

    fn notify(
        &self,
        person: [u8; 32],
        question: tilo_relay_protocol::certify::PendingConfirmation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let push = self.clone();
        Box::pin(async move {
            let agent = question.agent_label.trim();
            let title = if agent.is_empty() {
                "An agent needs you".to_owned()
            } else {
                format!("{agent} needs you")
            };
            let body = match (question.action_label.trim(), question.workspace.trim()) {
                ("", _) => "Open Tilo to answer.".to_owned(),
                (action, "") => format!("{action}. Open Tilo to answer."),
                (action, workspace) => format!("{action} in {workspace}. Open Tilo to answer."),
            };
            let tag = format!("tilo-question-{}", hex::encode(question.id));
            let delivered = push
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
            tracing::info!(
                delivered,
                "a person with no Tilo open was told a question waits"
            );
        })
    }
}

/// What a person's assistant told them, pushed to the browsers they
/// subscribed. A push message is encrypted for the browser it goes to, so
/// the push service between reads none of it.
#[cfg(feature = "agents")]
impl crate::workspace_telling::Outside for WebPush {
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
            let (title, body) = crate::workspace_telling::words_outside(&told);
            // One notification a thing: telling it again takes its place.
            let tag = format!("tilo-told-{}", told.id);
            let delivered = push
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
            u32::try_from(delivered).unwrap_or(u32::MAX)
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;
    use ring::{aead, agreement, hkdf};

    /// What a browser does with a push: agree on the key with its own
    /// private key and auth secret, and open the one record.
    fn decrypt(
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
}
