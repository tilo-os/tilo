//! Webhooks: a workspace's events posted to a URL, signed, in order, retried
//! until the URL takes them.
//!
//! Each webhook reads its workspace's events as the key that made it, with
//! the same watch the event stream holds ([`super::events::Watch`]), from
//! where it last delivered: the cursor is kept with the webhook, so a
//! restart neither loses events nor posts delivered ones again. A delivery is
//! `POST <url>` with the event as its body and
//!
//! - `Tilo-Webhook-Id`, the webhook;
//! - `Tilo-Delivery`, unique per event and webhook, the same on every retry;
//! - `Tilo-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">`
//!   under the webhook's secret.
//!
//! A key's webhooks are the account's (`account/webhooks#list`, `#create`, `#delete`): [`list`],
//! [`create`] and [`delete`] answer those actions.
//!
//! A `2xx` within [`ANSWER_WITHIN`] delivers it; anything else is tried again
//! after [`RETRIES`], and an event the URL never takes is given up on (and
//! logged) so the ones after it still go. Revoking the key ends its
//! webhooks.
//!
//! Whoever holds a key names the URL, so an API posts only where its own
//! machine and network are not: over `https` to the public internet, a name
//! judged in the connection's own look-up, no redirect followed and no proxy
//! of its environment taken. An API started inside a Tilo desktop also posts
//! over plain `http` to that computer's own loopback, for its person's
//! programs there ([`Posting`], [`refusal`]).

use super::answers::Event;
use crate::Server;
use crate::account::{Outcome, failed};
use crate::store::{Grant, Webhook};
use hmac::{KeyInit as _, Mac as _};
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// How long a URL has to answer one delivery.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);
/// The waits before each retry: 8 attempts over about two and a half hours.
pub const RETRIES: [Duration; 7] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
    Duration::from_secs(1800),
    Duration::from_secs(3600),
    Duration::from_secs(3600),
];
/// How long a look at the workspace waits for something to happen.
const LOOK_MS: u64 = 25_000;
/// How long a webhook waits before looking again after its workspace could
/// not be read.
const UNREADABLE: Duration = Duration::from_secs(30);

/// The signature header's value for `body` at `time` (Unix seconds).
#[must_use]
pub fn signature(secret: &str, time: u64, body: &[u8]) -> String {
    // An HMAC takes a key of any length, so this never refuses one.
    let Ok(mut mac) = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(time.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("t={time},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// What a hosted API says of a URL it posts no webhook to.
const PUBLIC_ONLY: &str = "a webhook posts to an https URL on the public internet";
/// What a desktop's API says of one.
const PUBLIC_OR_HERE: &str =
    "a webhook posts to an https URL on the public internet, or over plain http to this computer";
/// What either says of an address of its own written into a URL.
const OWN_ADDRESS: &str = "a webhook posts to the public internet, and this URL names an \
                           address of the API's own machine or network";
/// This computer's own name.
const LOCALHOST: &str = "localhost";

/// Why no webhook is posted to `url`, where none is. Every API posts over
/// `https` to the public internet: an address of its own machine or network
/// written into the URL is refused here, and a name is judged where the
/// connection looks it up ([`Posting`]). An API started inside a Tilo desktop
/// (`desktop`) also posts over plain `http` to that computer's own loopback,
/// for its person's programs there; a hosted one posts no plain `http` at all.
#[must_use]
pub fn refusal(url: &str, desktop: bool) -> Option<&'static str> {
    let unfit = if desktop { PUBLIC_OR_HERE } else { PUBLIC_ONLY };
    let Ok(url) = url::Url::parse(url) else {
        return Some(unfit);
    };
    let written = tilo_net::address::written_out(&url);
    match url.scheme() {
        "https" if written.is_some_and(|address| !tilo_net::address::is_public(address)) => {
            Some(OWN_ADDRESS)
        }
        "https" => None,
        "http" if desktop && loopback(&url, written) => None,
        _ => Some(unfit),
    }
}

/// Whether `url` names this computer's own loopback: by the address it
/// writes out (`written`), or by this computer's own name.
fn loopback(url: &url::Url, written: Option<IpAddr>) -> bool {
    match written {
        Some(address) => address.is_loopback(),
        None => url.domain() == Some(LOCALHOST),
    }
}

/// A look-up that answers this computer's own loopback for its own name, and
/// no other name at all.
fn this_computer() -> tilo_net::address::Lookup {
    Arc::new(|name: String| {
        Box::pin(async move {
            if name.eq_ignore_ascii_case(LOCALHOST) {
                Ok(vec![
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(Ipv6Addr::LOCALHOST),
                ])
            } else {
                Err(std::io::Error::other(format!(
                    "{name} is not this computer"
                )))
            }
        })
    })
}

/// The clients a running API posts webhooks with. Which it has is decided
/// once, by how the API was started ([`Server::new`], or [`Server::here`]
/// inside a desktop), and by nothing a request, a header or a key says.
#[derive(Clone)]
pub struct Posting {
    /// To the public internet: dialled directly, no redirect followed, and
    /// connected only where the one judge passed every address the name
    /// answered with, in the connection's own look-up.
    public: tilo_net::Client,
    /// Inside a desktop: plain `http` to that computer's own loopback.
    here: Option<tilo_net::Client>,
    /// The receivers on this machine a test has stand in for the public internet.
    #[cfg(feature = "test-support")]
    stand_ins: Arc<Mutex<Vec<std::net::SocketAddr>>>,
}

impl Posting {
    /// The clients of an API started inside a desktop (`desktop`), or of a
    /// hosted one.
    ///
    /// # Errors
    /// A client could not be built.
    pub fn new(desktop: bool) -> Result<Self, tilo_net::NetError> {
        // Whoever holds a key names the URL: no proxy of this API's
        // environment is taken, which would look the name up itself, behind
        // the judging.
        let client = || {
            tilo_net::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .redirects(tilo_net::Redirects::None)
                .direct()
        };
        let judged = tilo_net::address::public_only(tilo_net::address::system_lookup());
        Ok(Self {
            public: client().lookup(judged).build()?,
            here: desktop
                .then(|| client().lookup(this_computer()).build())
                .transpose()?,
            #[cfg(feature = "test-support")]
            stand_ins: Arc::default(),
        })
    }

    /// Why no webhook is posted to `url` by this API, where none is
    /// ([`refusal`]).
    #[must_use]
    pub fn refusal(&self, url: &str) -> Option<&'static str> {
        #[cfg(feature = "test-support")]
        if self.stands_in(url) {
            return None;
        }
        refusal(url, self.here.is_some())
    }

    /// The client `url` is posted with, or why it is posted to by none.
    fn client(&self, url: &str) -> Result<&tilo_net::Client, &'static str> {
        if let Some(why) = self.refusal(url) {
            return Err(why);
        }
        let plain = url::Url::parse(url).is_ok_and(|url| url.scheme() == "http");
        Ok(match &self.here {
            Some(here) if plain => here,
            _ => &self.public,
        })
    }
}

#[cfg(feature = "test-support")]
impl Posting {
    /// Have the receiver at `at` on this machine stand in for one on the public internet: a
    /// webhook to exactly that address and port, over plain `http`, is kept and posted. Every
    /// other URL is judged as on a real API, and no redirect is followed from a stand-in
    /// either. For a test, which starts its receiver itself.
    pub fn stand_in(&self, at: std::net::SocketAddr) {
        crate::lock(&self.stand_ins).push(at);
    }

    /// Whether `url` names a receiver a test has stand in: that receiver's own address with
    /// its port, as written.
    fn stands_in(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        let origin = url.origin().ascii_serialization();
        crate::lock(&self.stand_ins)
            .iter()
            .any(|stand_in| origin == format!("http://{stand_in}"))
    }
}

/// The kinds of event a webhook posts.
const KINDS: [&str; 3] = ["change", "invocation", "presence"];

/// `account/webhooks#list {workspace?}`: `grant`'s webhooks, each with why nothing is posted to
/// its URL where nothing is.
pub(crate) fn list(server: &Server, grant: &Grant, args: &Value) -> Outcome {
    let workspace = args.get("workspace").and_then(Value::as_str);
    let hooks: Vec<Value> = server
        .store
        .webhooks(None)
        .into_iter()
        .filter(|hook| hook.grant == grant.id && workspace.is_none_or(|ws| hook.workspace == ws))
        .map(|hook| row(&hook, false, server.posting.refusal(&hook.url)))
        .collect();
    Outcome::Ok(json!({"webhooks": hooks}))
}

/// `account/webhooks#create {workspace, url, events?, path?}`: a webhook of `grant`'s, delivered
/// from now on, answered with its secret.
pub(crate) fn create(server: &Arc<Server>, grant: &Grant, args: &Value) -> Outcome {
    let text = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_owned);
    let (Some(workspace), Some(url)) = (text("workspace"), text("url")) else {
        return failed(
            "invalid_arguments",
            "`workspace` and `url` are required",
            "pass {workspace, url, events?, path?}",
        );
    };
    if let Some(why) = server.posting.refusal(&url) {
        return failed(
            "invalid_arguments",
            why,
            "name an https URL on the public internet",
        );
    }
    let events: Vec<String> = args
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if let Some(unknown) = events.iter().find(|kind| !KINDS.contains(&kind.as_str())) {
        return failed(
            "invalid_arguments",
            format!("{unknown} is no kind of event"),
            "name change, invocation or presence",
        );
    }
    let (Ok(id), Ok(secret)) = (crate::store::secret(12), crate::store::secret(32)) else {
        return failed(
            "unavailable",
            "no randomness is available",
            "try again shortly",
        );
    };
    let hook = Webhook {
        id: format!("wh_{id}"),
        grant: grant.id.clone(),
        workspace,
        url,
        events,
        path: text("path"),
        secret,
        cursor: None,
    };
    if server.store.add_webhook(hook.clone()).is_err() {
        return failed(
            "unavailable",
            "the webhook could not be kept",
            "try again shortly",
        );
    }
    let made = row(&hook, true, None);
    Deliveries::start(server, hook);
    Outcome::Ok(made)
}

/// `account/webhooks#delete {webhook}`: stop posting to a webhook of `grant`'s.
pub(crate) fn delete(server: &Server, grant: &Grant, args: &Value) -> Outcome {
    let id = args
        .get("webhook")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match server.store.remove_webhook(&grant.id, id) {
        Ok(true) => {
            server.v1.deliveries.stop(id);
            Outcome::Ok(json!({"deleted": id}))
        }
        Ok(false) => failed(
            "not_found",
            "no such webhook of this key",
            "account/webhooks#list names them",
        ),
        Err(_) => failed(
            "unavailable",
            "the webhook could not be removed",
            "try again shortly",
        ),
    }
}

/// `hook` as its holder reads it: with its secret when it is made, and with why nothing is
/// posted to its URL (`undelivered`) where nothing is.
fn row(hook: &Webhook, with_secret: bool, undelivered: Option<&str>) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("id".into(), json!(hook.id));
    row.insert("workspace".into(), json!(hook.workspace));
    row.insert("url".into(), json!(hook.url));
    row.insert("events".into(), json!(hook.events));
    for (name, value) in [
        ("path", hook.path.as_deref()),
        ("secret", with_secret.then_some(hook.secret.as_str())),
        ("undelivered", undelivered),
    ] {
        if let Some(value) = value {
            row.insert(name.into(), json!(value));
        }
    }
    Value::Object(row)
}

/// The webhooks being delivered, by id.
#[derive(Default)]
pub struct Deliveries {
    running: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

impl Deliveries {
    /// Deliver every webhook the server keeps.
    pub fn start_all(server: &Arc<Server>) {
        for hook in server.store.webhooks(None) {
            Self::start(server, hook);
        }
    }

    /// Deliver `hook` from where it last delivered.
    pub fn start(server: &Arc<Server>, hook: Webhook) {
        let id = hook.id.clone();
        let task = tokio::spawn(deliver(Arc::downgrade(server), hook));
        let replaced = crate::lock(&server.v1.deliveries.running).insert(id, task.abort_handle());
        if let Some(replaced) = replaced {
            replaced.abort();
        }
    }

    /// Stop delivering `id`.
    pub fn stop(&self, id: &str) {
        let running = crate::lock(&self.running).remove(id);
        if let Some(running) = running {
            running.abort();
        }
    }
}

/// Deliver `hook`'s events until it, its key or the server is gone.
async fn deliver(server: Weak<Server>, hook: Webhook) {
    let mut watch = None;
    loop {
        let Some(live) = server.upgrade() else { return };
        let Some(grant) = live
            .store
            .grant(&hook.grant)
            .filter(crate::store::Grant::live)
        else {
            return;
        };
        if let Some(why) = live.posting.refusal(&hook.url) {
            // Kept before its URL was judged: nothing is read for it and
            // nothing is posted, and its holder reads why where their
            // webhooks are listed.
            tracing::info!(webhook = %hook.id, why, "a webhook is not delivered");
            return;
        }
        if watch.is_none() {
            match live.api_session(&grant).await {
                Ok(session) => {
                    watch = Some(super::events::Watch::new(
                        session,
                        hook.workspace.clone(),
                        hook.path.clone(),
                        live.store
                            .webhooks(None)
                            .into_iter()
                            .find(|kept| kept.id == hook.id)
                            .and_then(|kept| kept.cursor),
                    ));
                }
                Err(refused) => {
                    tracing::info!(webhook = %hook.id, reason = %refused.body.message, "a webhook's workspace cannot be read now");
                    drop(live);
                    tokio::time::sleep(UNREADABLE).await;
                    continue;
                }
            }
        }
        let posting = live.posting.clone();
        drop(live);
        let Some(reading) = watch.as_mut() else {
            continue;
        };
        let events = match reading.next(LOOK_MS).await {
            Ok(events) => events,
            Err(refused) => {
                tracing::info!(webhook = %hook.id, reason = %refused.body.message, "a webhook's workspace cannot be read now");
                watch = None;
                tokio::time::sleep(UNREADABLE).await;
                continue;
            }
        };
        for (index, event) in events.iter().enumerate() {
            if hook.events.is_empty() || hook.events.contains(&event.kind) {
                post(&posting, &hook, event, index).await;
            }
        }
        if !events.is_empty()
            && let (Some(server), Some(cursor)) = (server.upgrade(), reading.cursor())
            && let Err(error) = server.store.webhook_delivered(&hook.id, cursor)
        {
            tracing::warn!(webhook = %hook.id, %error, "where a webhook delivered up to was not kept");
        }
    }
}

/// Post `event` (the `index`th of its batch) to `hook`'s URL until it takes
/// it or the retries run out.
async fn post(posting: &Posting, hook: &Webhook, event: &Event, index: usize) {
    let body = serde_json::to_vec(event).unwrap_or_default();
    let delivery = format!("{}-{index}", event.id);
    for wait in std::iter::once(Duration::ZERO).chain(RETRIES) {
        tokio::time::sleep(wait).await;
        match attempt(posting, hook, &delivery, &body).await {
            Ok(status) if (200..300).contains(&status) => return,
            Ok(status) => {
                tracing::info!(webhook = %hook.id, status, "a webhook refused a delivery; it is tried again");
            }
            Err(error) => {
                tracing::info!(webhook = %hook.id, %error, "a webhook was not reached; it is tried again");
            }
        }
    }
    tracing::warn!(webhook = %hook.id, delivery, "a webhook never took a delivery; it was given up on");
}

/// One delivery of `body` to `hook`'s URL, as `delivery`: the status it
/// answered.
///
/// # Errors
/// The URL is not one this API posts to, a name in it answered with an
/// address of the API's own machine or network, or it was not reached.
pub(crate) async fn attempt(
    posting: &Posting,
    hook: &Webhook,
    delivery: &str,
    body: &[u8],
) -> Result<u16, tilo_net::NetError> {
    let http = posting
        .client(&hook.url)
        .map_err(|why| tilo_net::NetError::Blocked(why.to_owned()))?;
    let time = tilo_clock::unix_now_ms() / 1000;
    let answer = http
        .post(&hook.url)
        .header("content-type", "application/json")
        .header("tilo-webhook-id", hook.id.as_str())
        .header("tilo-delivery", delivery)
        .header("tilo-signature", signature(&hook.secret, time, body))
        .body(body.to_vec())
        .timeout(ANSWER_WITHIN)
        .send()
        .await?;
    Ok(answer.status().as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Store};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn the_signature_is_an_hmac_of_the_time_and_body() {
        // HMAC-SHA256("key", "1700000000.{}"), as Python's hmac computes it.
        let signed = signature("key", 1_700_000_000, b"{}");
        assert_eq!(
            signed,
            "t=1700000000,v1=9d713ed406bb7076d4123f0dc2c39d2df5c654ed4b0cd56b52c8b4c940bd63ae"
        );
        assert_ne!(signed, signature("other", 1_700_000_000, b"{}"));
        assert_ne!(signed, signature("key", 1_700_000_001, b"{}"));
    }

    /// Every form an address of this machine or its network takes in a URL.
    const OWN: [&str; 14] = [
        "https://127.0.0.1/hook",
        "https://127.0.0.1:8443/hook",
        "https://10.0.0.7/hook",
        "https://192.168.1.1/hook",
        "https://100.64.0.1/hook",
        "https://169.254.169.254/latest/meta-data",
        "https://168.63.129.16/machine",
        "https://0.0.0.0/hook",
        "https://[::1]/hook",
        "https://[::ffff:127.0.0.1]/hook",
        "https://[64:ff9b::a00:7]/hook",
        "https://[2002:a00:7::]/hook",
        "https://[fd00::7]/hook",
        "https://[fe80::1]/hook",
    ];

    /// An API as a site starts it: hosted.
    fn hosted() -> Arc<Server> {
        Server::new(
            &Config {
                issuer: "https://api.test".into(),
                web_entry: "https://tilo.test".into(),
                // Nothing here reaches a workspace, so nothing dials one.
                hosts: Vec::new(),
            },
            Store::in_memory(),
        )
        .unwrap()
    }

    /// A desktop that holds no workspace.
    struct Desktop;

    impl crate::session::Here for Desktop {
        fn workspaces(&self, _: &tilo_user_identity::DeviceCert) -> Vec<crate::session::Workspace> {
            Vec::new()
        }

        fn enter(
            &self,
            workspace: &str,
            _: &crate::grant_key::GrantKey,
        ) -> Result<crate::session::Reached, String> {
            Err(format!("no workspace {workspace} here"))
        }
    }

    /// An API as a Tilo desktop starts it, for its person's own programs.
    fn desktop() -> Arc<Server> {
        Server::here(Arc::new(Desktop)).unwrap()
    }

    fn hook(url: &str) -> Webhook {
        Webhook {
            id: "wh_1".into(),
            grant: "grant".into(),
            workspace: "workspace".into(),
            url: url.into(),
            events: Vec::new(),
            path: None,
            secret: "secret".into(),
            cursor: None,
        }
    }

    /// A listener on this machine that counts the connections made to it and
    /// closes each at once, so whoever connected is not kept waiting.
    struct Listening {
        port: u16,
        connections: Arc<AtomicUsize>,
    }

    impl Listening {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&connections);
            tokio::spawn(async move {
                while let Ok((connection, _)) = listener.accept().await {
                    counted.fetch_add(1, Ordering::SeqCst);
                    drop(connection);
                }
            });
            Self { port, connections }
        }

        fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }
    }

    /// What a delivery carried: its headers and its body.
    type Delivered = (hyper::HeaderMap, Vec<u8>);

    /// A webhook's receiver on this machine, over plain http: its port, and
    /// what each delivery carried. Its first answer is `first`, every later
    /// one `200`; with `to`, each answer also redirects there.
    async fn receiver(
        first: u16,
        to: Option<String>,
    ) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Delivered>) {
        use http_body_util::BodyExt as _;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, received) = tokio::sync::mpsc::unbounded_channel();
        let answered = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (sender, answered, to) = (sender.clone(), Arc::clone(&answered), to.clone());
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let (sender, answered, to) =
                                (sender.clone(), Arc::clone(&answered), to.clone());
                            async move {
                                let (parts, body) = request.into_parts();
                                let body = body.collect().await.unwrap().to_bytes().to_vec();
                                let _ = sender.send((parts.headers, body));
                                let status = if answered.fetch_add(1, Ordering::SeqCst) == 0 {
                                    first
                                } else {
                                    200
                                };
                                let mut answer = hyper::Response::builder().status(status);
                                if let Some(to) = to {
                                    answer = answer.header("location", to);
                                }
                                Ok::<_, std::convert::Infallible>(
                                    answer
                                        .body(http_body_util::Full::new(hyper::body::Bytes::new()))
                                        .unwrap(),
                                )
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        (port, received)
    }

    /// Whether `headers` sign `body` with `secret`.
    fn signed(headers: &hyper::HeaderMap, body: &[u8], secret: &str) -> bool {
        let said = headers["tilo-signature"].to_str().unwrap();
        let time: u64 = said
            .strip_prefix("t=")
            .and_then(|rest| rest.split(',').next())
            .and_then(|time| time.parse().ok())
            .unwrap();
        said == signature(secret, time, body)
    }

    /// A hosted API posts over https to the public internet and to nothing
    /// else: no plain http at all, and no address of its own machine or
    /// network in any form an address takes.
    #[test]
    fn a_hosted_api_posts_to_https_on_the_public_internet_and_to_nothing_else() {
        let api = hosted();
        let plain = [
            "http://127.0.0.1:8080/hook",
            "http://localhost:8080/hook",
            "http://[::1]:8080/hook",
            "http://example.com/hook",
            "file:///etc/passwd",
            "file://127.0.0.1/etc/passwd",
            "redis://127.0.0.1/",
            "blob:http://127.0.0.1/x",
            "not a url",
        ];
        for own in OWN.iter().chain(&plain) {
            assert!(
                api.posting.refusal(own).is_some(),
                "a hosted API posts a webhook to {own}"
            );
        }
        assert_eq!(api.posting.refusal("https://hooks.example/hook"), None);
    }

    /// Inside a desktop plain http to that computer's own loopback stays, and
    /// nothing else of its own machine or network is opened there.
    #[test]
    fn a_desktops_api_also_posts_plain_http_to_its_own_loopback_and_opens_nothing_else() {
        let api = desktop();
        for kept in [
            "http://127.0.0.1:8080/hook",
            "http://localhost:8080/hook",
            "http://[::1]:8080/hook",
            "https://hooks.example/hook",
        ] {
            assert_eq!(api.posting.refusal(kept), None, "{kept}");
        }
        let elsewhere = [
            "http://10.0.0.7/hook",
            "http://192.168.1.5:8080/hook",
            "http://169.254.169.254/latest/meta-data",
            "http://[::ffff:127.0.0.1]/hook",
            "http://example.com/hook",
            "file:///etc/passwd",
            "file://127.0.0.1/etc/passwd",
            "redis://127.0.0.1/",
            "blob:http://127.0.0.1/x",
        ];
        for own in OWN.iter().chain(&elsewhere) {
            assert!(
                api.posting.refusal(own).is_some(),
                "a desktop's API posts a webhook to {own}"
            );
        }
    }

    /// A name is judged where the connection looks it up: a delivery to a
    /// name that answers with an address of this machine connects to nothing.
    #[tokio::test]
    async fn a_delivery_to_a_name_that_answers_with_this_machine_connects_to_nothing() {
        let here = Listening::start().await;
        let api = hosted();
        let named = hook(&format!("https://localhost:{}/hook", here.port));

        let answered = attempt(&api.posting, &named, "d-1", b"{}").await;
        assert!(
            matches!(answered, Err(tilo_net::NetError::Blocked(_))),
            "{answered:?}"
        );
        assert_eq!(
            here.connections(),
            0,
            "a delivery to a name that answers with this machine connected to it"
        );
    }

    /// A webhook kept before its URL was judged is posted to nowhere by a
    /// hosted API: not over https to an own address, and not over plain http.
    #[tokio::test]
    async fn a_webhook_kept_earlier_that_names_this_machine_is_posted_to_nowhere() {
        let here = Listening::start().await;
        let api = hosted();
        for own in [
            format!("https://127.0.0.1:{}/hook", here.port),
            format!("http://127.0.0.1:{}/hook", here.port),
            format!("http://localhost:{}/hook", here.port),
        ] {
            let answered = attempt(&api.posting, &hook(&own), "d-1", b"{}").await;
            assert!(
                matches!(answered, Err(tilo_net::NetError::Blocked(_))),
                "{own}: {answered:?}"
            );
        }
        assert_eq!(
            here.connections(),
            0,
            "a webhook kept earlier that names this machine was posted to"
        );
    }

    /// A delivery takes no proxy from the API's environment: such a proxy
    /// would look the name up itself, behind the look-up a delivery is judged
    /// by, and so reach what that look-up refuses.
    #[tokio::test]
    async fn a_delivery_is_sent_to_no_proxy_the_environment_names() {
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "a_delivery_is_sent_to_no_proxy_the_environment_names",
        ) {
            return;
        }
        let proxy = Listening::start().await;
        let here = Listening::start().await;
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
        let api = hosted();
        let named = hook(&format!("https://localhost:{}/hook", here.port));

        let answered = attempt(&api.posting, &named, "d-1", b"{}").await;
        assert!(answered.is_err(), "{answered:?}");
        assert_eq!(
            proxy.connections(),
            0,
            "the proxy this API's environment names was sent a delivery's connection"
        );
        assert_eq!(
            here.connections(),
            0,
            "a delivery to a name that answers with this machine connected to it"
        );
    }

    /// Inside a desktop a delivery reaches its person's own program on that
    /// computer's loopback, by address and by name, signed with the webhook's
    /// secret; and a redirect that program answers is not followed.
    #[tokio::test]
    async fn a_desktops_api_delivers_to_its_own_loopback_signed_and_follows_no_redirect() {
        let api = desktop();
        let (port, mut deliveries) = receiver(200, None).await;
        for name in ["127.0.0.1", "localhost"] {
            let to = hook(&format!("http://{name}:{port}/hook"));
            let answered = attempt(&api.posting, &to, "d-1", b"{\"type\":\"change\"}").await;
            assert_eq!(answered, Ok(200), "{name}");
            let (headers, body) = deliveries.recv().await.unwrap();
            assert_eq!(body, b"{\"type\":\"change\"}");
            assert_eq!(headers["tilo-delivery"], "d-1");
            assert_eq!(headers["tilo-webhook-id"], "wh_1");
            assert!(signed(&headers, &body, "secret"), "{name}: {headers:?}");
        }

        let elsewhere = Listening::start().await;
        let other = format!("http://127.0.0.1:{}/hook", elsewhere.port);
        let (port, _redirected) = receiver(307, Some(other)).await;
        let to = hook(&format!("http://127.0.0.1:{port}/hook"));
        let answered = attempt(&api.posting, &to, "d-2", b"{}").await;
        assert!(
            !matches!(answered, Ok(200..=299)),
            "a redirect is no delivery: {answered:?}"
        );
        assert_eq!(elsewhere.connections(), 0, "a redirect was followed");
    }

    /// A delivery its URL refused is tried again the same: one delivery id,
    /// one body, signed with the webhook's secret.
    #[tokio::test]
    async fn a_delivery_its_url_refused_is_tried_again_the_same() {
        let api = desktop();
        let (port, mut deliveries) = receiver(500, None).await;
        let to = hook(&format!("http://127.0.0.1:{port}/hook"));
        let event = Event {
            id: "7".into(),
            kind: "invocation".into(),
            workspace: "workspace".into(),
            data: serde_json::json!({"invocation": "fill-1"}),
        };

        post(&api.posting, &to, &event, 0).await;
        let (first_headers, first_body) = deliveries.recv().await.unwrap();
        let (again_headers, again_body) = deliveries.recv().await.unwrap();
        assert_eq!(
            first_headers["tilo-delivery"],
            again_headers["tilo-delivery"]
        );
        assert_eq!(first_body, again_body);
        assert!(
            signed(&again_headers, &again_body, "secret"),
            "the delivery is signed with the webhook's secret"
        );
        let delivered: serde_json::Value = serde_json::from_slice(&again_body).unwrap();
        assert_eq!(
            (&delivered["type"], &delivered["workspace"]),
            (
                &serde_json::json!("invocation"),
                &serde_json::json!("workspace")
            ),
            "{delivered}"
        );
    }
}
