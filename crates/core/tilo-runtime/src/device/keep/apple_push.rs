//! Telling a person in an app of theirs that is closed, on Apple's systems.
//!
//! An app that is closed is told nothing unless Apple's push service
//! carries it there. A device of a person's hands this server the token
//! that service gave its app, with a key of the device's own
//! (`workspaces::push_addresses`). When a thing waits for a person and no
//! device of theirs is looking, the teller (`tilo_app_host::agents::telling::Teller`)
//! hands it here as it hands it to the sender for browsers, and one push
//! goes to each of that person's devices this server keeps an address for.
//!
//! A push carries as little as tells the device that something waits. In
//! the open, where Apple reads it: a line of the app's own that says
//! nothing, what kind of thing it is, and a name for the thing that only
//! that device can tie to it. The agent's name, the words and the buttons'
//! ids and labels are encrypted for the device's own key, with the code the
//! sender for browsers encrypts with, so neither Apple nor anything between
//! reads a word. Words that stay on the server are in no push at all,
//! encrypted or not. A push never holds
//! a key, a token, what a button invokes or its arguments, where the thing
//! is, a person's id or a workspace's id.
//!
//! The server signs in to Apple's service with a key Apple gave whoever
//! publishes the app. It is a file its owner alone reads, named when the
//! server starts, read here and never written, printed or logged. Without
//! one nothing is sent, and the server says so once.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::device::keep::web_push::PushSubscription;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper_util::rt::{TokioExecutor, TokioIo};
use ring::rand::SystemRandom;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_person_services::person_telling::Told;

use tilo_person_services::push_addresses::{PushAddress, PushEnvironment, PushKind};
use tilo_workspace_service::workspaces::Workspaces;

/// What this sender is called where a telling is written down on the thing
/// told.
const NAME: &str = "apns";
/// Where Apple's push service takes pushes for apps people installed.
const PRODUCTION: &str = "api.push.apple.com";
/// Where it takes them for apps built for development.
const DEVELOPMENT: &str = "api.sandbox.push.apple.com";
const PORT: u16 = 443;
/// The line of the app's own that a push shows where nothing else can be
/// shown. The app words it, and it says nothing of the thing.
const ALERT_KEY: &str = "TOLD_WAITS";
/// What the system groups the notifications of a person's assistant under.
const THREAD: &str = "assistant";
/// How long Apple's service keeps a push for a device that is off: as long
/// as the sender for browsers has a browser's kept.
const TTL_SECS: u64 = 10 * 60;
/// How long one sign-in token is used. Apple takes one for an hour.
const TOKEN_KEPT_SECS: u64 = 50 * 60;
/// How young a sign-in token is kept even when Apple did not take it:
/// Apple refuses a team that signs new ones more often.
const TOKEN_RENEWED_SECS: u64 = 20 * 60;
/// The longest the part for the device alone may be, before it is
/// encrypted.
const MAX_SEALED: usize = 2_000;
/// The longest a push may be, as Apple's service takes one.
const MAX_PUSH: usize = 4_096;
/// The longest name of an agent a push carries, in bytes.
const MAX_NAME_BYTES: usize = 120;
/// The longest answer read from Apple's service.
const MAX_ANSWER: usize = 4_096;
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const ANSWER_WAIT: Duration = Duration::from_secs(15);

/// What a server is told at its start about Apple's push service.
#[derive(Clone, Debug)]
pub struct ApplePushSetup {
    /// The file the key is in, as Apple hands it out (a `.p8` file). Its
    /// owner alone may read it.
    pub key_file: PathBuf,
    /// The key's id, as Apple's developer account lists it.
    pub key_id: String,
    /// The id of the team the key and the apps belong to.
    pub team_id: String,
    /// The apps a push is sent for, by their identifiers.
    pub topics: Vec<String>,
}

/// One end of Apple's push service.
#[derive(Clone)]
struct Reached {
    name: String,
    port: u16,
}

/// Where Apple's push service is reached, and what is trusted to be it.
#[derive(Clone)]
struct Service {
    production: Reached,
    development: Reached,
    tls: Arc<rustls::ClientConfig>,
}

impl Service {
    /// Apple's own, trusted as everything else this server reaches is.
    fn apple() -> Result<Self, String> {
        let mut tls = tilo_net::client_config().map_err(|error| error.to_string())?;
        tls.alpn_protocols = vec![b"h2".to_vec()];
        let reached = |name: &str| Reached {
            name: name.to_owned(),
            port: PORT,
        };
        Ok(Self {
            production: reached(PRODUCTION),
            development: reached(DEVELOPMENT),
            tls: Arc::new(tls),
        })
    }
}

/// An open connection pushes are sent on.
type Sender = hyper::client::conn::http2::SendRequest<Full<Bytes>>;

/// This server's sender for Apple's push service. A clone is the same
/// sender.
#[derive(Clone)]
pub struct ApplePush {
    inner: Arc<Inner>,
}

struct Inner {
    key: ring::signature::EcdsaKeyPair,
    key_id: String,
    team_id: String,
    topics: Vec<String>,
    workspaces: Workspaces,
    service: Service,
    /// The token Apple's service takes with each push, and the second it
    /// was signed.
    signed: std::sync::Mutex<Option<(String, u64)>>,
    production: tokio::sync::Mutex<Option<Sender>>,
    development: tokio::sync::Mutex<Option<Sender>>,
}

/// What a push is.
enum Push<'a> {
    /// One that shows: the thing known to the device as `name` waits.
    Alert { name: &'a str },
    /// One that shows nothing: a thing shown before waits no more.
    Gone,
}

/// What Apple's service did with a push.
enum Answer {
    Took,
    /// The address is gone, since the moment it names where it names one,
    /// in milliseconds since the epoch.
    Gone(Option<u64>),
    /// Refused, with the status and Apple's word for why.
    Refused(u16, String),
    Failed(String),
}

/// What Apple's service says when it does not take a push.
#[derive(Default, Deserialize)]
struct Refusal {
    #[serde(default)]
    reason: String,
    #[serde(default)]
    timestamp: Option<u64>,
}

/// What a push says to the device alone.
#[derive(Serialize)]
struct Sealed<'a> {
    from: &'a str,
    words: &'a str,
    actions: Vec<Button<'a>>,
}

/// One button of a thing, as a notification shows it: never what it
/// invokes, nor with what.
#[derive(Serialize)]
struct Button<'a> {
    id: &'a str,
    label: &'a str,
    main: bool,
}

/// The signing key in the file at `path`, which its owner alone may read.
/// Nothing of the file's contents is in what this answers.
fn read_key(path: &Path) -> Result<ring::signature::EcdsaKeyPair, String> {
    use rustls::pki_types::pem::PemObject as _;
    use std::os::unix::fs::MetadataExt as _;
    let unread = |error: std::io::Error| format!("{}: {error}", path.display());
    let metadata = std::fs::metadata(path).map_err(unread)?;
    if metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "{} can be read by others than its owner: give it mode 600",
            path.display()
        ));
    }
    let pem = std::fs::read(path).map_err(unread)?;
    let der = rustls::pki_types::PrivatePkcs8KeyDer::from_pem_slice(&pem)
        .map_err(|_| format!("{} holds no key as Apple hands one out", path.display()))?;
    ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        der.secret_pkcs8_der(),
        &SystemRandom::new(),
    )
    .map_err(|_| format!("{} holds no key as Apple hands one out", path.display()))
}

/// `text`, cut at a character to at most `most` bytes.
fn clip(text: &str, most: usize) -> &str {
    if text.len() <= most {
        return text;
    }
    let mut end = most;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// What the thing `id` is called in a push to the device at `address`: a
/// name only that device can tie to the thing, made with the device's own
/// secret ([`tilo_person_services::person_telling::known_as`], which the
/// device computes the same name with for what it shows itself).
fn name_for(address: &PushAddress, id: &str) -> Option<String> {
    let secret = hex::decode(&address.auth).ok()?;
    Some(tilo_person_services::person_telling::known_as(&secret, id))
}

/// What a push of `told` says to the device at `address` alone, encrypted
/// for that device's key: none where the words stay on the server.
fn sealed(told: &Told, address: &PushAddress) -> Option<String> {
    if !told.outside {
        return None;
    }
    let from = told.from.as_ref().map_or("", |from| from.name.trim());
    let from = clip(from, MAX_NAME_BYTES);
    let said = |actions: Vec<Button<'_>>| {
        serde_json::to_vec(&Sealed {
            from,
            words: &told.words,
            actions,
        })
        .ok()
    };
    let buttons = told
        .actions
        .iter()
        .map(|action| Button {
            id: &action.id,
            label: &action.label,
            main: action.main,
        })
        .collect();
    let mut plain = said(buttons)?;
    if plain.len() > MAX_SEALED {
        // Too long with its buttons: the device offers to open it.
        plain = said(Vec::new())?;
    }
    if plain.len() > MAX_SEALED {
        return None;
    }
    let (p256dh, auth) = address.subscription_key().ok()?;
    let device = PushSubscription {
        endpoint: String::new(),
        p256dh,
        auth,
    };
    let message = crate::device::keep::web_push::encrypt(&device, &plain).ok()?;
    Some(STANDARD.encode(message))
}

/// The push that shows the thing `told`, known to its device as `name`:
/// what Apple reads, and `enc`, which it cannot.
fn alert(told: &Told, name: &str, enc: Option<String>) -> Vec<u8> {
    let mut push = serde_json::json!({
        "aps": {
            "alert": {"loc-key": ALERT_KEY},
            "mutable-content": 1,
            "thread-id": THREAD,
            "interruption-level": "active",
        },
        "told": name,
        "seq": told.seq,
        "kind": told.kind,
        "raised": told.insists.map_or(1, |insists| insists.raised),
    });
    if let (Some(enc), Some(fields)) = (enc, push.as_object_mut()) {
        fields.insert("enc".to_owned(), enc.into());
    }
    push.to_string().into_bytes()
}

/// Open a connection to `reached` that pushes are sent on.
async fn connect(reached: &Reached, tls: &Arc<rustls::ClientConfig>) -> Result<Sender, String> {
    let dialled = tokio::net::TcpStream::connect((reached.name.as_str(), reached.port));
    let tcp = tokio::time::timeout(CONNECT_WAIT, dialled)
        .await
        .map_err(|_| "no connection in time".to_owned())?
        .map_err(|error| error.to_string())?;
    let name = rustls::pki_types::ServerName::try_from(reached.name.clone())
        .map_err(|error| error.to_string())?;
    let stream = tokio_rustls::TlsConnector::from(tls.clone())
        .connect(name, tcp)
        .await
        .map_err(|error| error.to_string())?;
    if stream.get_ref().1.alpn_protocol() != Some(b"h2".as_slice()) {
        return Err("the service does not speak HTTP/2".to_owned());
    }
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
            .await
            .map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(%error, "the connection to Apple's push service ended");
        }
    });
    Ok(sender)
}

impl ApplePush {
    /// The sender a server was told to be at its start. None where no key
    /// was named, and none where the key that was named cannot be used:
    /// the server's log says which, once.
    #[must_use]
    pub fn open(setup: Option<&ApplePushSetup>, workspaces: Workspaces) -> Option<Self> {
        let Some(setup) = setup else {
            tracing::info!(
                "no key for Apple's push service was named at the start: an app that is closed is told nothing"
            );
            return None;
        };
        match Service::apple().and_then(|service| Self::with(setup, workspaces, service)) {
            Ok(push) => {
                tracing::info!(
                    apps = ?setup.topics,
                    "what waits for a person is pushed to their closed apps through Apple's push service"
                );
                Some(push)
            }
            Err(reason) => {
                tracing::warn!(
                    %reason,
                    "the key for Apple's push service cannot be used: an app that is closed is told nothing"
                );
                None
            }
        }
    }

    /// The sender `setup` describes, pushing to `service`.
    fn with(
        setup: &ApplePushSetup,
        workspaces: Workspaces,
        service: Service,
    ) -> Result<Self, String> {
        let id = |id: &str| {
            let id = id.trim();
            let listed = !id.is_empty()
                && id.len() <= 32
                && id.bytes().all(|byte| byte.is_ascii_alphanumeric());
            listed.then(|| id.to_owned())
        };
        let (Some(key_id), Some(team_id)) = (id(&setup.key_id), id(&setup.team_id)) else {
            return Err(
                "the key's id and the team's id are named as Apple's developer account lists them"
                    .to_owned(),
            );
        };
        let topics: Vec<String> = setup
            .topics
            .iter()
            .map(|topic| topic.trim().to_owned())
            .filter(|topic| !topic.is_empty())
            .collect();
        if topics.is_empty() {
            return Err("no app is named to push for".to_owned());
        }
        let key = read_key(&setup.key_file)?;
        Ok(Self {
            inner: Arc::new(Inner {
                key,
                key_id,
                team_id,
                topics,
                workspaces,
                service,
                signed: std::sync::Mutex::default(),
                production: tokio::sync::Mutex::default(),
                development: tokio::sync::Mutex::default(),
            }),
        })
    }

    /// The token Apple's service takes with a push at `now_secs`: this
    /// team's, signed with its key, and used for most of the hour Apple
    /// takes one.
    fn bearer(&self, now_secs: u64) -> Option<String> {
        let mut signed = self.inner.signed.lock_recover();
        if let Some((token, at)) = signed.as_ref()
            && now_secs.saturating_sub(*at) < TOKEN_KEPT_SECS
        {
            return Some(token.clone());
        }
        let header = serde_json::json!({"alg": "ES256", "kid": self.inner.key_id});
        let claims = serde_json::json!({"iss": self.inner.team_id, "iat": now_secs});
        let signing = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self
            .inner
            .key
            .sign(&SystemRandom::new(), signing.as_bytes())
            .ok()?;
        let token = format!("{signing}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()));
        *signed = Some((token.clone(), now_secs));
        drop(signed);
        Some(token)
    }

    /// Apple's service did not take the sign-in token at `now_secs`: the
    /// next push signs a new one, unless this one is younger than Apple
    /// lets a team renew its token.
    fn renew_bearer(&self, now_secs: u64) {
        let mut signed = self.inner.signed.lock_recover();
        if signed
            .as_ref()
            .is_some_and(|(_, at)| now_secs.saturating_sub(*at) >= TOKEN_RENEWED_SECS)
        {
            *signed = None;
        }
    }

    /// A connection to the end of Apple's service that takes pushes for
    /// `environment`: the one that is open, or a new one.
    async fn connected(&self, environment: PushEnvironment) -> Result<Sender, String> {
        let (reached, open) = match environment {
            PushEnvironment::Production => (&self.inner.service.production, &self.inner.production),
            PushEnvironment::Development => {
                (&self.inner.service.development, &self.inner.development)
            }
        };
        let mut open = open.lock().await;
        if let Some(sender) = open.as_ref().filter(|sender| !sender.is_closed()) {
            return Ok(sender.clone());
        }
        let sender = connect(reached, &self.inner.service.tls).await?;
        *open = Some(sender.clone());
        drop(open);
        Ok(sender)
    }

    /// The request that sends `body` as `push` to the device at `address`,
    /// signed in with `bearer` at `now_secs`.
    fn request(
        &self,
        address: &PushAddress,
        environment: PushEnvironment,
        push: &Push<'_>,
        body: Vec<u8>,
        bearer: &str,
        now_secs: u64,
    ) -> Result<hyper::Request<Full<Bytes>>, String> {
        let reached = match environment {
            PushEnvironment::Production => &self.inner.service.production,
            PushEnvironment::Development => &self.inner.service.development,
        };
        let authority = if reached.port == PORT {
            reached.name.clone()
        } else {
            format!("{}:{}", reached.name, reached.port)
        };
        let (kind, priority) = match push {
            Push::Alert { .. } => ("alert", "10"),
            Push::Gone => ("background", "5"),
        };
        let mut request = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri(format!("https://{authority}/3/device/{}", address.token))
            .header("authorization", format!("bearer {bearer}"))
            .header("apns-topic", address.topic.as_str())
            .header("apns-push-type", kind)
            .header("apns-priority", priority)
            .header(
                "apns-expiration",
                now_secs.saturating_add(TTL_SECS).to_string(),
            )
            .header("content-type", "application/json");
        if let Push::Alert { name } = push {
            // A thing raised again takes the place of itself.
            request = request.header("apns-collapse-id", *name);
        }
        request
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| error.to_string())
    }

    /// Send `body` as `push` to the device at `address`.
    async fn post(&self, address: &PushAddress, push: &Push<'_>, body: Vec<u8>) -> Answer {
        let now_secs = tilo_clock::unix_now_ms() / 1000;
        let Some(environment) = address.environment else {
            return Answer::Failed("an Apple push address names its environment".to_owned());
        };
        let Some(bearer) = self.bearer(now_secs) else {
            return Answer::Failed("the push could not be signed".to_owned());
        };
        let request = match self.request(address, environment, push, body, &bearer, now_secs) {
            Ok(request) => request,
            Err(reason) => return Answer::Failed(reason),
        };
        let mut sender = match self.connected(environment).await {
            Ok(sender) => sender,
            Err(reason) => return Answer::Failed(reason),
        };
        let response = match tokio::time::timeout(ANSWER_WAIT, sender.send_request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return Answer::Failed(error.to_string()),
            Err(_) => return Answer::Failed("no answer in time".to_owned()),
        };
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Answer::Took;
        }
        let said = Limited::new(response.into_body(), MAX_ANSWER)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
            .unwrap_or_default();
        let refusal = if said.is_empty() {
            Refusal::default()
        } else {
            serde_json::from_slice(&said).unwrap_or_else(|error| {
                tracing::debug!(%error, status, "Apple's push service answered what does not read");
                Refusal::default()
            })
        };
        if status == 410 {
            return Answer::Gone(refusal.timestamp);
        }
        if status == 403 {
            self.renew_bearer(now_secs);
        }
        // Apple's words for a refusal are single words: anything else is
        // not passed on.
        let worded = refusal.reason.len() <= 64
            && refusal
                .reason
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric());
        Answer::Refused(
            status,
            if worded {
                refusal.reason
            } else {
                String::new()
            },
        )
    }

    /// What `answer` says of a push to `address`, a device of `person`'s:
    /// whether the device took it. An address Apple says is gone is
    /// forgotten; nothing that reaches a device is logged.
    fn took(&self, person: &[u8; 32], address: &PushAddress, answer: Answer) -> bool {
        match answer {
            Answer::Took => return true,
            Answer::Gone(since) => {
                tilo_person_services::PersonServices::of(&self.inner.workspaces)
                    .forget_push_address(person, &address.token, since);
            }
            Answer::Refused(status, reason) => {
                tracing::warn!(status, %reason, "Apple's push service refused a push");
            }
            Answer::Failed(reason) => {
                tracing::debug!(%reason, "a push to a person's device failed");
            }
        }
        false
    }

    /// Push the thing `told` to every device of `person`'s this server
    /// keeps an address for. Answers how many took it.
    async fn told(&self, person: &[u8; 32], told: &Told) -> u32 {
        // Nothing is pushed for a thing that does not wait for its person.
        if !told.waits() {
            return 0;
        }
        let services = tilo_person_services::PersonServices::of(&self.inner.workspaces);
        let mut took: Vec<String> = Vec::new();
        for address in services.push_addresses(person) {
            if address.kind != PushKind::Apns || !self.inner.topics.contains(&address.topic) {
                continue;
            }
            let Some(name) = name_for(&address, &told.id) else {
                continue;
            };
            let mut body = alert(told, &name, sealed(told, &address));
            if body.len() > MAX_PUSH {
                body = alert(told, &name, None);
            }
            let answer = self
                .post(&address, &Push::Alert { name: &name }, body)
                .await;
            if self.took(person, &address, answer) {
                took.push(address.token);
            }
        }
        services.push_shown(person, &took, &told.id);
        u32::try_from(took.len()).unwrap_or(u32::MAX)
    }

    /// The things `ids` wait for `person` no more: every device a push
    /// showed one of them on is sent one push that shows nothing and names
    /// the thing as gone, once.
    async fn gone(&self, person: &[u8; 32], ids: &[String]) {
        let services = tilo_person_services::PersonServices::of(&self.inner.workspaces);
        for (address, shown) in services.push_taken_back(person, ids, PushKind::Apns) {
            if address.kind != PushKind::Apns || !self.inner.topics.contains(&address.topic) {
                continue;
            }
            for id in shown {
                let Some(name) = name_for(&address, &id) else {
                    continue;
                };
                let body = serde_json::json!({
                    "aps": {"content-available": 1},
                    "told": name,
                    "gone": true,
                });
                let answer = self
                    .post(&address, &Push::Gone, body.to_string().into_bytes())
                    .await;
                self.took(person, &address, answer);
            }
        }
    }
}

/// What a person's assistant told them, pushed to the apps of theirs that
/// are closed.
impl tilo_app_host::agents::telling::Outside for ApplePush {
    fn name(&self) -> &'static str {
        NAME
    }

    fn tell(&self, person: [u8; 32], told: Told) -> Pin<Box<dyn Future<Output = u32> + Send>> {
        let push = self.clone();
        Box::pin(async move { push.told(&person, &told).await })
    }

    fn taken_back(
        &self,
        person: [u8; 32],
        ids: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let push = self.clone();
        Box::pin(async move { push.gone(&person, &ids).await })
    }
}

#[cfg(test)]
mod tests;
