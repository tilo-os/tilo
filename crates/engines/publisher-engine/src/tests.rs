//! The engine's ops, as the App Publisher asks them: this machine's key, its signatures, and the
//! registry applications it makes. The registry is a stub on this machine's own loopback, so no
//! test reaches the published one.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use app_registry::publisher::key;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use registry_types::application::PublisherApplication;
use serde_json::Value;
use tilo_secrets::SecretStore;
use tilolang_engine::Provider as _;

use crate::{APP, Publisher};

/// A machine of its own: a home whose environment names no key, and secrets kept in memory.
struct Machine {
    _home: tilo_test_env::TestHome,
    engine: Publisher,
}

impl Machine {
    fn new() -> Self {
        let mut home = tilo_test_env::TestHome::new().expect("a test home");
        home.env().remove(key::KEY_ENV);
        let engine = Publisher::with_store(SecretStore::memory(APP));
        Self {
            _home: home,
            engine,
        }
    }

    fn call(&self, op: &str, input: &str) -> Result<Value, String> {
        self.engine
            .call(op, input)
            .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
    }
}

/// A registry on this machine's loopback: it answers `replies` in turn and keeps every request.
struct Registry {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// One request the stub registry read: its opening line and its body.
#[derive(Clone)]
struct Seen {
    line: String,
    body: String,
}

impl Registry {
    /// A registry answering `replies`, one request each, then closing.
    fn answering(replies: &[&'static str]) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback registry");
        let addr = listener.local_addr().expect("its address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let replies: Vec<&'static str> = replies.to_vec();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().expect("the request"));
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let mut length = 0_usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header == "\r\n" {
                        break;
                    }
                    if let Some((_, value)) = header.split_once(':')
                        && header.to_ascii_lowercase().starts_with("content-length:")
                    {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0_u8; length];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                recorded.lock().expect("the stub's log").push(Seen {
                    line: line.trim_end().to_owned(),
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        Self { addr, seen }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the stub's log").clone()
    }
}

/// The moves one job tells.
type Moves = std::sync::mpsc::Receiver<(tilolang_engine::State, Value)>;

/// One job started on `machine`'s engine, with the moves it tells.
fn start(machine: &Machine, op: &str, ask: &str) -> Result<Moves, String> {
    use tilolang_engine::JobSink;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let job = format!(
        "job-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let (tell, moves) = std::sync::mpsc::channel();
    let tell = std::sync::Mutex::new(tell);
    machine.engine.start(
        &job,
        op,
        ask,
        JobSink::new(move |state, detail| {
            let _ = tell
                .lock()
                .expect("the test's channel")
                .send((state, detail));
        }),
    )?;
    Ok(moves)
}

/// What the job came to: its last move, its state among its fields.
fn outcome(moves: &Moves) -> Value {
    loop {
        let (state, mut detail) = moves
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the job ends");
        if state.ended() {
            detail["state"] = Value::from(state.word());
            return detail;
        }
    }
}

#[test]
fn the_key_is_made_once_and_only_its_public_half_answers() {
    let machine = Machine::new();
    let before = machine.call("key-state", "").expect("the state");
    assert_eq!(before["state"], "missing");
    assert_eq!(before["public"], "");

    let made = machine.call("create", "").expect("a key is made");
    assert_eq!(made["state"], "present");
    let public = made["public"].as_str().expect("the public key").to_owned();
    assert_eq!(public.len(), 64, "an Ed25519 public key, as hex");

    let again = machine
        .call("create", "")
        .expect_err("a key is never replaced");
    assert!(again.contains("already has a publisher key"), "{again}");

    let after = machine.call("key-state", "").expect("the state");
    assert_eq!(after["state"], "present");
    assert_eq!(after["public"], public.as_str());
}

#[test]
fn a_machine_without_a_key_answers_no_proof_and_no_key_file() {
    let machine = Machine::new();
    for op in ["key-file", "sign"] {
        let input = if op == "sign" {
            r#"{"message_hex":"00"}"#
        } else {
            ""
        };
        let refused = machine.call(op, input).expect_err("there is no key");
        assert!(refused.contains("no publisher key"), "{op}: {refused}");
    }
}

#[test]
fn the_key_file_is_what_tilolang_exports() {
    let machine = Machine::new();
    machine.call("create", "").expect("a key is made");
    let file = machine
        .call("key-file", "")
        .expect("the key file")
        .get("text")
        .and_then(Value::as_str)
        .expect("its text")
        .to_owned();
    let read = key::parse(&file).expect("the key file reads");
    let public = machine
        .call("key-state", "")
        .expect("the state")
        .get("public")
        .and_then(Value::as_str)
        .expect("the public key")
        .to_owned();
    assert_eq!(key::public_hex(&read), public);
    assert!(file.contains("secret_key_hex"), "{file}");
}

#[test]
fn signing_happens_inside_the_engine() {
    let machine = Machine::new();
    let made = machine.call("create", "").expect("a key is made");
    let public = made["public"].as_str().expect("the public key").to_owned();
    let message = b"a package's manifest".to_vec();
    let input = serde_json::json!({ "message_hex": hex::encode(&message) }).to_string();
    let signed = machine.call("sign", &input).expect("the message is signed");
    let signature = signed["signature_hex"].as_str().expect("the signature");
    let bytes: [u8; 64] = hex::decode(signature)
        .expect("hex")
        .try_into()
        .expect("64 bytes");
    let key = VerifyingKey::from_bytes(
        &<[u8; 32]>::try_from(hex::decode(&public).expect("hex")).expect("32 bytes"),
    )
    .expect("a verifying key");
    key.verify(&message, &Signature::from_bytes(&bytes))
        .expect("the engine signed with the machine's key");

    let bad = machine
        .call("sign", r#"{"message_hex":"zz"}"#)
        .expect_err("that is no hexadecimal");
    assert!(bad.contains("not hexadecimal"), "{bad}");
}

#[test]
fn an_application_is_signed_with_the_device_key_and_its_namespaces() {
    let machine = Machine::new();
    let made = machine.call("create", "").expect("a key is made");
    let public = made["public"].as_str().expect("the public key").to_owned();
    let registry = Registry::answering(&[r#"{"status":"pending","namespaces":["com.example"]}"#]);
    let ask = serde_json::json!({
        "registry": registry.url(),
        "domain": "example.com",
        "contact": "ada@example.com",
    })
    .to_string();
    let started = start(&machine, "apply", &ask).expect("the application starts");
    let ended = outcome(&started);
    assert_eq!(ended["state"], "done", "{ended}");
    assert_eq!(ended["summary"], "pending (com.example)");

    let seen = registry.seen();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].line.starts_with("POST /publishers/applications "),
        "{:?}",
        seen[0].line
    );
    let application: PublisherApplication =
        serde_json::from_str(&seen[0].body).expect("a signed application");
    let namespaces = application.verify().expect("the application verifies");
    assert_eq!(namespaces, ["com.example"]);
    assert_eq!(application.pubkey_hex, public);
    assert_eq!(application.contact, "ada@example.com");
}

#[test]
fn the_registrys_answer_about_the_key_reads_back() {
    let machine = Machine::new();
    let made = machine.call("create", "").expect("a key is made");
    let public = made["public"].as_str().expect("the public key").to_owned();
    let registry = Registry::answering(&[
        r#"{"status":"approved","namespaces":["com.example","io.github.ada"]}"#,
    ]);
    let ask = serde_json::json!({ "registry": registry.url() }).to_string();
    let started = start(&machine, "status", &ask).expect("the check starts");
    let ended = outcome(&started);
    assert_eq!(ended["state"], "done", "{ended}");
    assert_eq!(ended["summary"], "approved (com.example, io.github.ada)");
    let seen = registry.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].line,
        format!("GET /publishers/applications/{public} HTTP/1.1")
    );
}

#[test]
fn a_key_the_registry_knows_nothing_of_reads_as_no_application() {
    let machine = Machine::new();
    machine.call("create", "").expect("a key is made");
    let registry = Registry::answering(&[r#"{"status":"none"}"#]);
    let ask = serde_json::json!({ "registry": registry.url() }).to_string();
    let started = start(&machine, "status", &ask).expect("the check starts");
    let ended = outcome(&started);
    assert_eq!(ended["state"], "done", "{ended}");
    assert_eq!(ended["summary"], "none ()");
}

#[test]
fn an_application_that_claims_nothing_is_refused_before_a_call() {
    let machine = Machine::new();
    machine.call("create", "").expect("a key is made");
    let ask = serde_json::json!({
        "registry": "http://127.0.0.1:9",
        "domain": " ",
        "forge_account": "",
    })
    .to_string();
    let refused = start(&machine, "apply", &ask).expect_err("nothing is claimed");
    assert!(
        refused.contains("name a domain or a forge account"),
        "{refused}"
    );

    let forge = serde_json::json!({
        "registry": "http://127.0.0.1:9",
        "forge_account": "nowhere:ada",
    })
    .to_string();
    let refused = start(&machine, "apply", &forge).expect_err("no forge");
    assert!(refused.contains("unknown forge"), "{refused}");
}

#[test]
fn a_registry_is_https_or_this_machine() {
    let machine = Machine::new();
    machine.call("create", "").expect("a key is made");
    for address in [
        "http://apps.tilo.so",
        "http://localhost.example.com",
        "https://",
        "file:///tmp/registry",
        "not a URL",
    ] {
        let ask = serde_json::json!({ "registry": address, "domain": "example.com" }).to_string();
        let refused = start(&machine, "apply", &ask).expect_err("an address the client refuses");
        assert!(
            refused.contains("registry") || refused.contains("URL"),
            "{address}: {refused}"
        );
    }
    for address in [
        "http://127.0.0.1:9",
        "http://localhost:9",
        "https://apps.tilo.so",
    ] {
        let ask = serde_json::json!({ "registry": address, "domain": "example.com" }).to_string();
        assert!(
            start(&machine, "apply", &ask).is_ok(),
            "a registry the client accepts: {address}"
        );
    }
}

#[test]
fn an_unreachable_registry_fails_the_job_with_its_address() {
    let machine = Machine::new();
    machine.call("create", "").expect("a key is made");
    let ask = serde_json::json!({
        "registry": "http://127.0.0.1:9",
        "domain": "example.com",
        "contact": "ada@example.com",
    })
    .to_string();
    let started = start(&machine, "apply", &ask).expect("the application starts");
    let ended = outcome(&started);
    assert_eq!(ended["state"], "failed", "{ended}");
    assert!(
        ended["error"]
            .as_str()
            .is_some_and(|why| why.contains("127.0.0.1:9")),
        "{ended}"
    );
}

#[test]
fn a_job_this_device_does_not_know_is_refused() {
    let machine = Machine::new();
    assert!(!machine.engine.cancel("apply-999"));
}

#[test]
fn no_other_app_is_answered_and_no_other_op() {
    let machine = Machine::new();
    let refused = machine
        .engine
        .call("sign", "{}")
        .expect_err("a message is needed");
    assert!(refused.contains("does not read"), "{refused}");
    let unknown = machine.engine.call("publish", "").expect_err("no such op");
    assert!(unknown.contains("no op this device runs for"), "{unknown}");
    let unknown = start(&machine, "publish", "").expect_err("no such op");
    assert!(unknown.contains("no op this device runs for"), "{unknown}");
}

#[test]
fn the_engine_registers_for_the_app_the_cli_reads() {
    assert_eq!(APP, "so.tilo.publisher");
    assert_eq!(APP, key::SECRETS_APP);
}
