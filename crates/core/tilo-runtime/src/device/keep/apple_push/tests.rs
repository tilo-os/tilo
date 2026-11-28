//! What Apple's push service is sent, shown against a stand-in for it that
//! each test starts on this machine: what waits for a person is pushed once
//! to their devices and nobody else's, what does not reach them never, a
//! thing seen elsewhere is taken back, and the words a person keeps on the
//! server are in no push. Nothing here is real: every key, token and person
//! is made up when a test starts, and nothing leaves this machine.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "every test here keeps its server alive to the end"
)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;

use base64::Engine as _;
use http_body_util::BodyExt as _;
use hyper::body::Incoming;
use hyper::{Request, Response};
use ring::agreement;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};
use tilo_person_services::person_telling::{Cause, Kind, Teller as ToldBy};

use super::*;
use tilo_app_host::agents::telling::{LivePhase, LiveTurn, Outside as _, Teller};
use tilo_person_services::PersonServices;
use tilo_person_services::ToTell;
use tilo_person_services::push_addresses::LiveAddress;
use tilo_person_services::push_addresses::{PushKey, Register};
use tilo_workspace_service::workspaces::fixtures::{hosting_device, test_host};

/// A person this server keeps a log for. No device of theirs is on.
pub(crate) const ADA: [u8; 32] = [8; 32];
/// Another, who shares nothing with her.
const BO: [u8; 32] = [9; 32];
/// The workspace her assistant works in.
const WORKSPACE: &str = "6f1f0f7e-3c1d-4a54-9b6e-2d1c5a7e9f10";
/// The app the made-up tokens were given to.
const APP: &str = "so.tilo.chat";
const SECOND: u64 = 1_000;
/// A moment and a minute, in seconds, for what is counted in seconds.
const START: u64 = 1_000_000;
const MINUTE: u64 = 60;

/// One push the stand-in was sent.
pub(crate) struct Seen {
    /// The token it was sent to, as the address of the request names it.
    pub(crate) token: String,
    headers: HashMap<String, String>,
    body: Value,
}

impl Seen {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// Everything Apple could read of it, as one text.
    fn in_the_open(&self) -> String {
        let mut open = self.body.clone();
        open.as_object_mut().unwrap().remove("enc");
        format!("{} {:?} {open}", self.token, self.headers)
    }
}

/// What the stand-in answers a push to one token with, where it does not
/// take it: the status, and Apple's words for it.
type Answers = Arc<Mutex<HashMap<String, (u16, Value)>>>;

/// A stand-in for Apple's push service on this machine: HTTP/2 over TLS,
/// with a certificate made when it starts. It takes every push with 200
/// unless a test says what it answers one token with.
pub(crate) struct StandIn {
    port: u16,
    certificate: CertificateDer<'static>,
    seen: Arc<Mutex<Vec<Seen>>>,
    answers: Answers,
}

impl StandIn {
    pub(crate) async fn start() -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap()
            .der()
            .clone();
        let mut tls = rustls::ServerConfig::builder_with_provider(tilo_net::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.clone()],
                PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        tls.alpn_protocols = vec![b"h2".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let answers = Answers::default();
        let (heard, said) = (seen.clone(), answers.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, heard, said) = (acceptor.clone(), heard.clone(), said.clone());
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        answer(request, heard.clone(), said.clone())
                    });
                    let served = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                    drop(served);
                });
            }
        });
        Self {
            port,
            certificate,
            seen,
            answers,
        }
    }

    /// Apple's push service as a sender reaches this stand-in: at its port, trusting the
    /// certificate it made.
    pub(crate) fn service(&self) -> Service {
        let mut tls =
            tilo_net::client_config_also_trusting(vec![self.certificate.clone()]).unwrap();
        tls.alpn_protocols = vec![b"h2".to_vec()];
        let reached = Reached {
            name: "localhost".to_owned(),
            port: self.port,
        };
        Service {
            production: reached.clone(),
            development: reached,
            tls: Arc::new(tls),
        }
    }

    /// The pushes it was sent since this was last asked, in order.
    pub(crate) fn heard(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    /// Answer every push to `device` with `status` and `said` from now on.
    fn answers(&self, device: &Device, status: u16, said: Value) {
        self.answers
            .lock()
            .unwrap()
            .insert(device.token.clone(), (status, said));
    }
}

/// Answer one push as Apple's service would, and remember it.
async fn answer(
    request: Request<Incoming>,
    seen: Arc<Mutex<Vec<Seen>>>,
    answers: Answers,
) -> Result<Response<Full<Bytes>>, Infallible> {
    assert_eq!(request.method(), hyper::Method::POST);
    let token = request
        .uri()
        .path()
        .strip_prefix("/3/device/")
        .unwrap()
        .to_owned();
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.to_str().unwrap().to_owned()))
        .collect();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let (status, said) = answers
        .lock()
        .unwrap()
        .get(&token)
        .cloned()
        .unwrap_or((200, Value::Null));
    seen.lock().unwrap().push(Seen {
        token,
        headers,
        body,
    });
    let said = if said.is_null() {
        Bytes::new()
    } else {
        Bytes::from(said.to_string())
    };
    Ok(Response::builder()
        .status(status)
        .body(Full::new(said))
        .unwrap())
}

/// A device as an app makes itself one: the token a push service would
/// give it, and a key and a secret of its own. All made up.
pub(crate) struct Device {
    /// What the transport would certify it as.
    id: [u8; 32],
    pub(crate) token: String,
    key: agreement::EphemeralPrivateKey,
    public: Vec<u8>,
    secret: [u8; 16],
}

impl Device {
    pub(crate) fn new(n: u8) -> Self {
        let key =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &SystemRandom::new())
                .unwrap();
        let public = key.compute_public_key().unwrap().as_ref().to_vec();
        Self {
            id: [n; 32],
            token: format!("{n:02x}").repeat(32),
            key,
            public,
            secret: [n; 16],
        }
    }

    /// What it hands the server.
    fn handed_over(&self, environment: PushEnvironment) -> Register {
        Register::Apns {
            token: self.token.clone(),
            environment,
            topic: APP.to_owned(),
            key: PushKey {
                p256dh: hex::encode(&self.public),
                auth: hex::encode(self.secret),
            },
            live: None,
        }
    }

    /// What it hands the server of its live activities: the push-to-start token
    /// and, once one runs, the token it is updated under.
    fn handed_over_live(&self, (start, update, turn): (&str, &str, &str)) -> Register {
        Register::Apns {
            token: self.token.clone(),
            environment: PushEnvironment::Production,
            topic: APP.to_owned(),
            key: PushKey {
                p256dh: hex::encode(&self.public),
                auth: hex::encode(self.secret),
            },
            live: Some(LiveAddress {
                start: start.to_owned(),
                update: update.to_owned(),
                turn: turn.to_owned(),
            }),
        }
    }

    /// The name this device knows the thing `id` by, worked out as an app
    /// works it out: from the id and its own secret.
    fn name_of(&self, id: &str) -> String {
        tilo_person_services::person_telling::known_as(&self.secret, id)
    }

    /// What this device reads in the part of `push` that is for it alone.
    fn opened(self, push: &Seen) -> Value {
        let enc = push.body["enc"]
            .as_str()
            .or_else(|| push.body["aps"]["content-state"]["enc"].as_str())
            .unwrap();
        let enc = STANDARD.decode(enc).unwrap();
        let plain = crate::device::keep::web_push::tests::decrypt(
            self.key,
            &self.public,
            &self.secret,
            &enc,
        );
        serde_json::from_slice(&plain).unwrap()
    }
}

/// A key as Apple hands one out, made up now, in a file under `folder`
/// that its owner alone reads; and its public half.
pub(crate) fn made_up_key(folder: &Path) -> (ApplePushKey, Vec<u8>) {
    use ring::signature::KeyPair as _;
    use std::os::unix::fs::PermissionsExt as _;
    let algorithm = &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING;
    let random = SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(algorithm, &random).unwrap();
    let key =
        ring::signature::EcdsaKeyPair::from_pkcs8(algorithm, pkcs8.as_ref(), &random).unwrap();
    let encoded = STANDARD.encode(pkcs8.as_ref());
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", // a placeholder frame: the key in it is made up when the test starts
        lines.join("\n")
    );
    let key_file = folder.join("AuthKey_MADEUP0001.p8");
    std::fs::write(&key_file, pem).unwrap();
    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let setup = ApplePushKey {
        key_file,
        key_id: "MADEUP0001".to_owned(),
        team_id: "MADEUPTEAM".to_owned(),
        topics: vec![APP.to_owned()],
    };
    (setup, key.public_key().as_ref().to_vec())
}

/// The sender `setup` describes, pushing to the stand-in and trusting the
/// certificate it made.
fn for_stand_in(
    setup: &ApplePushKey,
    workspaces: Workspaces,
    apple: &StandIn,
) -> Result<ApplePush, String> {
    ApplePush::with(setup, workspaces, apple.service())
}

/// A server with nobody's device on, a stand-in for Apple's push service,
/// and the sender and the teller that push to it.
struct Served<S> {
    workspaces: Workspaces,
    apple: StandIn,
    push: ApplePush,
    teller: Teller,
    /// The public half of the made-up key the sender signs in with.
    signs_with: Vec<u8>,
    _server: S,
    _folders: (tempfile::TempDir, tempfile::TempDir),
}

async fn served() -> Served<impl Sized> {
    let apple = StandIn::start().await;
    let (state, keys) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (key, cert) = hosting_device();
    let server = test_host(state.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    // Her workspace, whose vault keeps her model's key.
    workspaces
        .create_workspace("Ada's", Some(WORKSPACE))
        .await
        .unwrap();
    let (setup, signs_with) = made_up_key(keys.path());
    let push = for_stand_in(&setup, workspaces.clone(), &apple).unwrap();
    let teller = Teller::new(workspaces.clone(), Arc::new(push.clone()));
    Served {
        workspaces,
        apple,
        push,
        teller,
        signs_with,
        _server: server,
        _folders: (state, keys),
    }
}

/// Hand `device` over as a device of `person`'s.
pub(crate) fn reach(workspaces: &Workspaces, person: &[u8; 32], device: &Device) {
    PersonServices::of(workspaces)
        .register_push_address(
            person,
            &device.id,
            device.handed_over(PushEnvironment::Production),
        )
        .unwrap();
}

/// The tokens the server keeps for `person`, the newest first.
fn reached(workspaces: &Workspaces, person: &[u8; 32]) -> Vec<String> {
    PersonServices::of(workspaces)
        .push_addresses(person)
        .into_iter()
        .map(|address| address.token)
        .collect()
}

fn assistant() -> ToldBy {
    ToldBy {
        agent: "assistant".into(),
        name: "Assistant".into(),
    }
}

/// A reminder a person's assistant tells them.
pub(crate) fn reminder(words: &str, key: &str) -> ToTell {
    ToTell {
        kind: Kind::Reminder,
        words: words.into(),
        cause: Some(Cause {
            kind: "reminder".into(),
            label: words.into(),
            key: key.into(),
        }),
        from: Some(assistant()),
        ..ToTell::default()
    }
}

/// News her assistant tells her.
fn news(words: &str, key: &str) -> ToTell {
    ToTell {
        kind: Kind::News,
        words: words.into(),
        cause: Some(Cause {
            kind: "event".into(),
            label: "Mail arrives".into(),
            key: key.into(),
        }),
        from: Some(assistant()),
        ..ToTell::default()
    }
}

/// What `authorization` signs in with: the header and the claims of its
/// token, once the made-up key `public` is seen to have signed them.
pub(crate) fn signed_in(authorization: &str, public: &[u8]) -> (Value, Value) {
    let token = authorization.strip_prefix("bearer ").unwrap();
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let signed = format!("{}.{}", parts[0], parts[1]);
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, public)
        .verify(
            signed.as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
        .unwrap();
    let read = |part: &str| -> Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    };
    (read(parts[0]), read(parts[1]))
}

/// What a push that shows a thing says in the open, and nothing more.
fn in_the_open(name: &str, told: &Told, kind: &str, raised: u32) -> Value {
    json!({
        "aps": {
            "alert": {"loc-key": "TOLD_WAITS"},
            "mutable-content": 1,
            "thread-id": "assistant",
            "interruption-level": "active",
        },
        "told": name,
        "seq": told.seq,
        "kind": kind,
        "raised": raised,
    })
}

#[tokio::test]
async fn a_thing_that_reaches_a_person_is_pushed_once_and_news_never() {
    let served = served().await;
    let workspaces = &served.workspaces;
    let phone = Device::new(1);
    reach(workspaces, &ADA, &phone);
    let reminded = PersonServices::of(workspaces)
        .tell(
            &ADA,
            reminder("The landlord writes about the lease.", "lease-1"),
        )
        .unwrap();
    let unread = PersonServices::of(workspaces)
        .tell(&ADA, news("A newsletter arrived.", "mail-2"))
        .unwrap();
    assert!(reminded.reaches);
    assert!(!unread.reaches);

    // Four seconds on, with no device of hers looking: the reminder is
    // pushed to her phone, once, and the news that reaches nobody is not.
    let due = unread.at_ms.saturating_add(4 * SECOND);
    assert_eq!(served.teller.step(due).await, None);
    let heard = served.apple.heard();
    assert_eq!(heard.len(), 1);
    let push = heard.first().unwrap();
    let name = phone.name_of(&reminded.id);
    assert_eq!(push.token, phone.token);
    assert_eq!(push.header("apns-topic"), Some(APP));
    assert_eq!(push.header("apns-push-type"), Some("alert"));
    assert_eq!(push.header("apns-priority"), Some("10"));
    assert_eq!(push.header("apns-collapse-id"), Some(name.as_str()));
    let (header, claims) = signed_in(push.header("authorization").unwrap(), &served.signs_with);
    assert_eq!(header, json!({"alg": "ES256", "kid": "MADEUP0001"}));
    assert_eq!(claims["iss"], "MADEUPTEAM");
    assert!(claims["iat"].is_u64());

    // In the open it says that something waits, and nothing of what: no
    // word of it, not its id nor what caused it, nobody's name, no
    // workspace and no person.
    let mut open = push.body.clone();
    assert!(open.as_object_mut().unwrap().remove("enc").is_some());
    assert_eq!(open, in_the_open(&name, &reminded, "reminder", 1));
    let read = push.in_the_open();
    let ada = hex::encode(ADA);
    for kept_back in [
        "landlord",
        "lease",
        "Assistant",
        "lease-1",
        reminded.id.as_str(),
        WORKSPACE,
        ada.as_str(),
    ] {
        assert!(!read.contains(kept_back), "{kept_back} in {read}");
    }
    // The rest is for her phone alone: who tells and the words.
    assert_eq!(
        phone.opened(push),
        json!({
            "from": "Assistant",
            "words": "The landlord writes about the lease.",
            "actions": [],
        })
    );

    // That it was pushed is written down on the thing, and it is pushed
    // never a second time, however long it waits.
    let log = PersonServices::of(workspaces).told_after(&ADA, 0).told;
    let pushed: Vec<Vec<(&str, u32)>> = log
        .iter()
        .map(|told| {
            told.pushed
                .iter()
                .map(|pushed| (pushed.by.as_str(), pushed.took))
                .collect()
        })
        .collect();
    assert_eq!(pushed, [vec![("apns", 1)], vec![]]);
    for later in [5 * SECOND, 60 * SECOND, 3_600 * SECOND] {
        assert_eq!(served.teller.step(due.saturating_add(later)).await, None);
    }
    assert!(served.apple.heard().is_empty());
}

#[tokio::test]
async fn a_thing_seen_on_another_device_is_taken_back_once() {
    let served = served().await;
    let workspaces = &served.workspaces;
    let (phone, laptop) = (Device::new(1), Device::new(2));
    reach(workspaces, &ADA, &phone);
    reach(workspaces, &ADA, &laptop);
    let told = PersonServices::of(workspaces)
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();
    let due = told.at_ms.saturating_add(4 * SECOND);
    assert_eq!(served.teller.step(due).await, None);
    assert_eq!(served.apple.heard().len(), 2);
    // Each device knows the thing by a name of its own.
    let names = [phone.name_of(&told.id), laptop.name_of(&told.id)];
    assert_ne!(names[0], names[1]);

    // She opens it on her laptop. Every device that showed it is sent one
    // push that shows nothing and names the thing as gone, and that is all
    // it says.
    PersonServices::of(workspaces)
        .acknowledge_told(&ADA, &told.id, "Ada's laptop", "opened")
        .unwrap();
    assert_eq!(served.teller.step(due.saturating_add(SECOND)).await, None);
    let gone = served.apple.heard();
    assert_eq!(gone.len(), 2);
    for (device, name) in [&phone, &laptop].into_iter().zip(&names) {
        let push = gone.iter().find(|push| push.token == device.token).unwrap();
        assert_eq!(
            push.body,
            json!({"aps": {"content-available": 1}, "told": name, "gone": true})
        );
        assert_eq!(push.header("apns-push-type"), Some("background"));
        assert_eq!(push.header("apns-priority"), Some("5"));
        assert_eq!(push.header("apns-collapse-id"), None);
    }

    // Once: nothing is taken back a second time.
    let later = due.saturating_add(2 * SECOND);
    assert_eq!(served.teller.step(later).await, None);
    assert!(served.apple.heard().is_empty());
}

#[tokio::test]
async fn words_that_stay_on_the_server_are_in_no_push() {
    let served = served().await;
    let workspaces = &served.workspaces;
    let phone = Device::new(1);
    reach(workspaces, &ADA, &phone);
    let kept = PersonServices::of(workspaces)
        .tell(
            &ADA,
            ToTell {
                words_stay: true,
                ..reminder("Sign the term sheet before the board meets.", "r-1")
            },
        )
        .unwrap();
    let free = PersonServices::of(workspaces)
        .tell(&ADA, reminder("Call Jo.", "r-2"))
        .unwrap();
    assert!(!kept.outside && free.outside);

    let due = free.at_ms.saturating_add(4 * SECOND);
    assert_eq!(served.teller.step(due).await, None);
    let heard = served.apple.heard();
    assert_eq!(heard.len(), 2);
    let of = |told: &Told| {
        heard
            .iter()
            .find(|push| push.body["seq"] == told.seq)
            .unwrap()
    };

    // The kept one is pushed with what is said in the open and nothing
    // else: no part for the device, encrypted or not, and no word of it.
    let quiet = of(&kept);
    assert_eq!(
        quiet.body,
        in_the_open(&phone.name_of(&kept.id), &kept, "reminder", 1)
    );
    let read = format!("{:?} {}", quiet.headers, quiet.body);
    for word in ["Sign", "term sheet", "board meets", WORKSPACE] {
        assert!(!read.contains(word), "{word} in {read}");
    }
    // The other one's words are for her phone alone.
    let aloud = of(&free);
    assert!(!aloud.in_the_open().contains("Call Jo."));
    assert_eq!(
        phone.opened(aloud),
        json!({"from": "Assistant", "words": "Call Jo.", "actions": []})
    );
}

#[tokio::test]
async fn a_push_for_one_person_never_reaches_another_persons_device() {
    let served = served().await;
    let workspaces = &served.workspaces;
    let (hers, his) = (Device::new(1), Device::new(2));
    reach(workspaces, &ADA, &hers);
    reach(workspaces, &BO, &his);
    // Each is told one thing, and both things have the same id, each in
    // its own person's log.
    let for_ada = PersonServices::of(workspaces)
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();
    let for_bo = PersonServices::of(workspaces)
        .tell(&BO, reminder("Pay the rent.", "r-1"))
        .unwrap();
    assert_eq!(for_ada.id, for_bo.id);

    let due = for_bo.at_ms.saturating_add(4 * SECOND);
    assert_eq!(served.teller.step(due).await, None);
    let heard = served.apple.heard();
    assert_eq!(heard.len(), 2);
    let to = |device: &Device| -> Vec<&Seen> {
        heard
            .iter()
            .filter(|push| push.token == device.token)
            .collect()
    };
    let (to_hers, to_his) = (to(&hers), to(&his));
    assert_eq!((to_hers.len(), to_his.len()), (1, 1));
    // Her device is pushed her thing, under her device's name for it, and
    // his is pushed his.
    assert_eq!(
        to_hers[0].header("apns-collapse-id"),
        Some(hers.name_of(&for_ada.id).as_str())
    );
    assert_eq!(
        to_his[0].header("apns-collapse-id"),
        Some(his.name_of(&for_bo.id).as_str())
    );
    assert_eq!(hers.opened(to_hers[0])["words"], "Call Jo.");
    assert_eq!(his.opened(to_his[0])["words"], "Pay the rent.");
}

#[tokio::test]
async fn the_sender_is_off_without_a_key_and_takes_no_key_others_can_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let served = served().await;
    let workspaces = &served.workspaces;
    // No key named at the start: no sender.
    assert!(ApplePush::open(None, workspaces.clone()).is_none());

    let folder = tempfile::tempdir().unwrap();
    let (setup, _) = made_up_key(folder.path());
    let opened = |setup: &ApplePushKey| for_stand_in(setup, workspaces.clone(), &served.apple);
    assert!(opened(&setup).is_ok());

    // A key others can read is not taken. What the server says of it names
    // the file and holds nothing that is in it.
    let mode = |mode: u32| {
        std::fs::set_permissions(&setup.key_file, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    mode(0o640);
    let refused = opened(&setup).err().unwrap();
    assert!(refused.contains("mode 600"), "{refused}");
    let written = std::fs::read_to_string(&setup.key_file).unwrap();
    for line in written.lines() {
        assert!(!refused.contains(line), "{refused}");
    }
    mode(0o600);
    assert!(opened(&setup).is_ok());

    // An id that is none, and no app to push for.
    let no_id = ApplePushKey {
        key_id: " ".to_owned(),
        ..setup.clone()
    };
    assert!(opened(&no_id).is_err());
    let no_app = ApplePushKey {
        topics: Vec::new(),
        ..setup.clone()
    };
    assert!(opened(&no_app).is_err());

    // A file that holds no key, and no file.
    std::fs::write(&setup.key_file, "no key at all").unwrap();
    let refused = opened(&setup).err().unwrap();
    assert!(refused.contains("holds no key"), "{refused}");
    std::fs::remove_file(&setup.key_file).unwrap();
    assert!(opened(&setup).is_err());

    // And with no sender nothing is pushed: what waits is told nowhere.
    let reminded = PersonServices::of(workspaces)
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();
    assert!(reminded.waits());
    assert!(served.apple.heard().is_empty());
}

#[tokio::test]
async fn an_address_apples_service_says_is_gone_is_forgotten_and_a_refused_one_is_kept() {
    let served = served().await;
    let workspaces = &served.workspaces;
    let (phone, laptop, tablet) = (Device::new(1), Device::new(2), Device::new(3));
    reach(workspaces, &ADA, &phone);
    reach(workspaces, &ADA, &laptop);
    PersonServices::of(workspaces)
        .register_push_address(
            &ADA,
            &tablet.id,
            tablet.handed_over(PushEnvironment::Development),
        )
        .unwrap();
    // The phone's app was removed since; the laptop's token is refused.
    let since = tilo_clock::unix_now_ms().saturating_add(60 * SECOND);
    served.apple.answers(
        &phone,
        410,
        json!({"reason": "Unregistered", "timestamp": since}),
    );
    served
        .apple
        .answers(&laptop, 400, json!({"reason": "BadDeviceToken"}));

    let told = PersonServices::of(workspaces)
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();
    let due = told.at_ms.saturating_add(4 * SECOND);
    assert_eq!(served.teller.step(due).await, None);
    // Every device was tried and one took it, which is what is written
    // down.
    assert_eq!(served.apple.heard().len(), 3);
    let log = PersonServices::of(workspaces).told_after(&ADA, 0).told;
    let pushed = log.first().unwrap().pushed.first().unwrap();
    assert_eq!((pushed.by.as_str(), pushed.took), ("apns", 1));
    // The address that is gone is forgotten. The refused one is kept: its
    // device hands over a new one when its app next starts.
    assert_eq!(
        reached(workspaces, &ADA),
        [tablet.token.clone(), laptop.token.clone()]
    );
}

#[tokio::test]
async fn what_must_not_be_missed_is_pushed_again_in_its_own_place() {
    let served = served().await;
    let workspaces = &served.workspaces;
    let phone = Device::new(1);
    reach(workspaces, &ADA, &phone);
    let mut must = reminder("File the tax return.", "tax-1");
    must.insists = true;
    let told = PersonServices::of(workspaces).tell(&ADA, must).unwrap();
    let again = told.insists.unwrap().next_at_ms.unwrap();

    let due = told.at_ms.saturating_add(4 * SECOND);
    assert_eq!(served.teller.step(due).await, Some(again));
    let first = served.apple.heard();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].body["raised"], 1);

    // Ten minutes on it is raised again, and pushed under the same name,
    // so it takes the place of itself on the device.
    served.teller.step(again).await;
    let second = served.apple.heard();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].body["raised"], 2);
    assert_eq!(second[0].body["told"], first[0].body["told"]);
    let name = phone.name_of(&told.id);
    assert_eq!(second[0].header("apns-collapse-id"), Some(name.as_str()));
    assert_eq!(first[0].header("apns-collapse-id"), Some(name.as_str()));
}

#[tokio::test]
async fn one_sign_in_token_is_used_for_most_of_an_hour() {
    let served = served().await;
    let first = served.push.bearer(START).unwrap();
    assert_eq!(served.push.bearer(START + 49 * MINUTE).unwrap(), first);
    let later = served.push.bearer(START + 51 * MINUTE).unwrap();
    assert_ne!(later, first);
    let (_, claims) = signed_in(&format!("bearer {later}"), &served.signs_with);
    assert_eq!(claims["iat"], START + 51 * MINUTE);

    // A token Apple did not take is kept while it is younger than Apple
    // lets a team renew one, and signed anew after.
    served.push.renew_bearer(START + 60 * MINUTE);
    assert_eq!(served.push.bearer(START + 60 * MINUTE).unwrap(), later);
    served.push.renew_bearer(START + 72 * MINUTE);
    assert_ne!(served.push.bearer(START + 72 * MINUTE).unwrap(), later);
}

/// The Swift notice extension's fixture names its thing as this host names it. The fixture
/// (`dist/apple/Tests/ShellCoreTests/Fixtures/SealedPush.json`) was sealed by this sender once,
/// and the Swift test opens it with the device's key.
#[test]
fn the_swift_notice_fixture_names_its_thing_as_this_host_names_it() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../dist/apple/Tests/ShellCoreTests/Fixtures/SealedPush.json");
    let fixture: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let kept = hex::decode(fixture["kept"].as_str().unwrap()).unwrap();
    assert_eq!(
        tilo_person_services::person_telling::known_as(&kept[32..], "reminder:ws-1:r_1"),
        fixture["told"].as_str().unwrap()
    );
}
fn live_turn(phase: LivePhase, step: u32, steps: u32) -> LiveTurn {
    LiveTurn {
        id: "turn-1".to_owned(),
        workspace: WORKSPACE.to_owned(),
        conversation: format!("{APP}/r-1"),
        agent: "scout".to_owned(),
        asked: "Summarise the lease.".to_owned(),
        phase,
        step,
        steps,
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one turn from where it starts to where it ends, in order"
)]
async fn a_turns_start_progress_and_end_reach_the_live_activity() {
    let served = served().await;
    let phone = Device::new(1);
    let start = "ab".repeat(32);
    PersonServices::of(&served.workspaces)
        .register_push_address(&ADA, &phone.id, phone.handed_over_live((&start, "", "")))
        .unwrap();
    let mut turn = live_turn(LivePhase::Working, 0, 0);
    let name = phone.name_of(&turn.id);
    served.push.live(ADA, turn.clone()).await;
    let heard = served.apple.heard();
    let began = heard.first().unwrap();
    assert_eq!(began.token, start);
    assert_eq!(began.header("apns-push-type"), Some("liveactivity"));
    assert_eq!(began.body["aps"]["event"], "start");
    // In the open it says nothing of what it works on: no word of the ask, of
    // the agent, of the workspace, nor of where a press leads.
    let readable = began.in_the_open();
    for kept_back in ["Summarise", "lease", "scout", WORKSPACE, "tilo-shell"] {
        assert!(!readable.contains(kept_back), "{kept_back} in {readable}");
    }
    // The device alone reads who works, what on, and where a press leads.
    assert_eq!(
        phone.opened(began),
        json!({
            "from": "scout",
            "asked": "Summarise the lease.",
            "words": "",
            "opens": format!("tilo-shell://turn?workspace={WORKSPACE}&app={APP}&record=r-1"),
        })
    );

    // The activity runs: its device hands over the token it is updated under.
    let (phone, update) = (Device::new(1), "cd".repeat(32));
    PersonServices::of(&served.workspaces)
        .register_push_address(
            &ADA,
            &phone.id,
            phone.handed_over_live((&start, &update, &name)),
        )
        .unwrap();
    turn.step = 2;
    turn.steps = 5;
    served.push.live(ADA, turn.clone()).await;
    let moved = served.apple.heard();
    assert_eq!(moved[0].token, update);
    assert_eq!(moved[0].body["aps"]["event"], "update");
    assert_eq!(moved[0].body["aps"]["content-state"]["step"], 2);

    // It asked the person something, and waits for them.
    turn.phase = LivePhase::Asking;
    served.push.live(ADA, turn.clone()).await;
    let asking = served.apple.heard();
    assert_eq!(asking[0].body["aps"]["content-state"]["phase"], "asking");

    // It answered, and the activity ends.
    turn.phase = LivePhase::Answered;
    served.push.live(ADA, turn).await;
    let ended = served.apple.heard();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].token, update);
    assert_eq!(ended[0].body["aps"]["event"], "end");
    assert_eq!(ended[0].body["aps"]["content-state"]["phase"], "answered");
    assert!(ended[0].body["aps"]["dismissal-date"].is_u64());
}
