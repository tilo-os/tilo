//! A device's daemon as a test's machine runs it.
//!
//! A machine with an identity enrolled on it runs one process that makes and takes the device key
//! ([`crate::persist::take_device_key`]), and every program of the machine reads the device
//! through it ([`DeviceActions::here`]): nothing else opens the key. A test whose machine has no
//! real daemon starts this one: it takes the machine's key, and answers the device actions that
//! need nothing but the key, at the endpoint the machine's programs resolve.
//!
//! Setup asks a daemon more than that: who the account is to it (`account#who`), founding a person
//! on a computer that has none, and logging it in with a ticket; [`DeviceDaemon::logging_in`]
//! answers those as a daemon does.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::Value;
use tilo_user_identity::cert::{DeviceCapabilities, DeviceCert, sign_device_cert};

use crate::IdentityPaths;
use crate::login::daemon::{DeviceActions, home_answer};
use crate::login::{Device, Place, file_certificate};
use crate::persist::{self, DeviceKeyLock};
use tilo_files::native::socket::{self, Listener};

/// The action [`DeviceActions::device_key`] asks: what this device is.
const INFO: &str = "info";

/// What the native endpoint is asked for who the asking account is.
const WHO: &str = "account#who";

/// The workspace of the person a fixture login enters, as its device names it.
const WORKSPACE: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

/// The person a fixture login's computer belonged to before the login, as `devices#found` makes
/// one: a person of the computer's own, whom the login replaces.
const FOUNDED: [u8; 32] = [0x17; 32];
/// The person whose root a fixture login's device printed the code: it certifies the computer.
const PRINTED: [u8; 32] = [0x42; 32];

/// How a daemon answers the `devices#logIn` setup makes.
#[derive(Clone, Copy, Debug)]
pub enum Login {
    /// Certify this computer, as the device that printed the code does.
    Certify,
    /// Refuse it with the message, as the device of a spent code does.
    Refuse(&'static str),
    /// Wait before certifying it, as a device a person has to approve by hand does.
    Wait,
    /// Answer the login as pending, as a daemon does while the person decides on another of
    /// their devices, and certify the computer once the call is asked again.
    Pending,
    /// [`Self::Pending`], refused with the message once the call is asked again.
    PendingRefused(&'static str),
    /// Answer the login as pending however often it is asked: a person who never answers it.
    PendingForever,
}

/// What the daemon answers besides its key.
struct Answers {
    /// The login a ticket's certificate is filed in, made on the caller's thread: a daemon's own
    /// thread reads none of the test's environment.
    place: Option<Place>,
    /// How it answers a login, or `None` for a machine whose setup asks none.
    login: Option<Login>,
    /// Whether this computer keeps the person's root, which `account#who` says.
    holds_root: bool,
    /// The device this computer is as it enters its person's workspaces, once a login made it.
    home: Mutex<Option<Device>>,
    /// How the login the person has not answered yet was asked, kept to settle the call.
    asking: Mutex<Option<Value>>,
}

/// The device's daemon of a test's machine, for as long as it lives: it holds the machine's
/// device key, and answers its programs' device actions at the endpoint they resolve.
#[must_use = "the daemon answers for as long as it lives"]
pub struct DeviceDaemon {
    /// The key the daemon took, and signs with while it answers.
    key: DeviceKeyLock,
    /// The endpoint the machine's programs reach this daemon at.
    native: PathBuf,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DeviceDaemon {
    /// The daemon of the machine whose device key and certificates are `paths`: it makes the
    /// machine's key when it has none (as `tilo-device-key-init` does at boot), takes it, and
    /// answers at [`DeviceActions::here`]'s endpoint for `paths`.
    ///
    /// # Errors
    /// The key cannot be made or is taken by another process, or the endpoint cannot be bound.
    pub fn start(paths: &IdentityPaths) -> std::io::Result<Self> {
        Self::answering(paths, None, false)
    }

    /// [`start`](Self::start) for a computer this account's person was made on: `account#who`
    /// says it keeps the person's root, which a backup is made of.
    ///
    /// # Errors
    /// As [`start`](Self::start).
    pub fn holding_the_root(paths: &IdentityPaths) -> std::io::Result<Self> {
        Self::answering(paths, None, true)
    }

    /// [`start`](Self::start) for the daemon of a computer setup adds to the person's devices:
    /// it answers a ticket's login with `login`, and says whether it keeps the person's root.
    ///
    /// # Errors
    /// As [`start`](Self::start).
    pub fn logging_in(
        paths: &IdentityPaths,
        login: Login,
        holds_root: bool,
    ) -> std::io::Result<Self> {
        Self::answering(paths, Some(login), holds_root)
    }

    fn answering(
        paths: &IdentityPaths,
        login: Option<Login>,
        holds_root: bool,
    ) -> std::io::Result<Self> {
        let refused = |error: persist::PersistError| std::io::Error::other(error.to_string());
        persist::load_or_create_device_key(paths).map_err(refused)?;
        let native = DeviceActions::here(paths.clone())
            .map_err(std::io::Error::other)?
            .native()
            .to_path_buf();
        // Bound before the key is taken: a machine whose endpoint a live daemon holds refuses
        // here, rather than this process holding the key of a daemon that is not here.
        let listener = socket::bind(&native)?;
        // Looked at, not blocked on: the daemon's thread sees the test's stop within milliseconds.
        #[cfg(unix)]
        listener.set_nonblocking(true)?;
        let key = persist::take_device_key(paths).map_err(refused)?;
        let signing = key.key().clone();
        let place = login
            .map(|_| {
                Place::here(paths.clone())
                    .map_err(|error| std::io::Error::other(format!("{error:#}")))
            })
            .transpose()?;
        let answers = Arc::new(Answers {
            place,
            login,
            holds_root,
            home: Mutex::new(None),
            asking: Mutex::new(None),
        });
        let stopping = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stopping);
        let thread = std::thread::spawn(move || serve(&listener, &answers, &signing, &stopped));
        Ok(Self {
            key,
            native,
            stopping,
            thread: Some(thread),
        })
    }

    /// This device's public key: the one its daemon signs with.
    #[must_use]
    pub fn device_key(&self) -> [u8; 32] {
        self.key.key().verifying_key().to_bytes()
    }

    /// The device actions of this daemon, as the machine's programs ask them: at its endpoint.
    #[must_use]
    pub fn actions(&self) -> DeviceActions {
        DeviceActions::at(self.native.clone())
    }
}

impl Drop for DeviceDaemon {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        #[cfg(not(unix))]
        let _ = socket::connect(&self.native);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // A socket's name stays behind after it closes on Unix; the next daemon there takes it.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.native);
    }
}

/// Answer this daemon's questions with `key` until `stopping` says the test is done. Each
/// connection is answered on a thread of its own, as a daemon serves its programs, and it too
/// stops answering once the daemon has.
fn serve(
    listener: &Listener,
    answers: &Arc<Answers>,
    key: &SigningKey,
    stopping: &Arc<AtomicBool>,
) {
    for stream in listener.incoming() {
        if stopping.load(Ordering::Relaxed) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            Err(_) => return,
        };
        let answers = Arc::clone(answers);
        let key = key.clone();
        let stopping = Arc::clone(stopping);
        let _spawned = std::thread::Builder::new()
            .name("fixture-device-daemon".into())
            .spawn(move || {
                // A call is followed on its connection, so a followed call is asked again.
                let mut lines = std::io::BufReader::new(&stream);
                let mut request = String::new();
                while !stopping.load(Ordering::Relaxed)
                    && lines.read_line(&mut request).is_ok_and(|read| read > 0)
                {
                    let _ = writeln!(&stream, "{}", answer(&answers, &request, &key));
                    request.clear();
                }
            });
    }
}

/// The daemon's answer, as the device action service writes it ([`crate::login::daemon`]), to the
/// request line `request`.
fn answer(answers: &Answers, request: &str, key: &SigningKey) -> Value {
    let asked: Value = serde_json::from_str(request).unwrap_or_default();
    let id = asked.get("id").cloned().unwrap_or(Value::Null);
    if asked.get("native").and_then(Value::as_str) == Some(WHO) {
        return who(answers, key);
    }
    // A login the person has not answered yet: the call is answered as pending, and settles once
    // it is asked again, as the daemon's is while the person decides on another of their devices.
    if let Some(login @ (Login::Pending | Login::PendingRefused(_) | Login::PendingForever)) =
        answers.login
    {
        let arguments = asked
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_default();
        let asking = arguments.get("invocation_id").is_none()
            && arguments
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| path.ends_with(crate::login::LOG_IN));
        if asking {
            *lock(&answers.asking) = Some(arguments);
        }
        if matches!(login, Login::PendingForever) || asking {
            let said = "pending 1: your person has not answered yet";
            return serde_json::json!({"jsonrpc": "2.0", "id": &id, "result": {
                "content": [{"type": "text", "text": said}],
                "structuredContent": {"status": "pending", "invocation_id": "1",
                    "waiting_for": "your person to answer"}}});
        }
        let asked = lock(&answers.asking).take().unwrap_or_default();
        let args = asked.get("args").cloned().unwrap_or_default();
        return match log_in(answers, &args, key) {
            Ok(result) => succeeded(&id, &result),
            Err(message) => failed(&id, &message),
        };
    }
    let path = asked
        .pointer("/params/arguments/path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = asked
        .pointer("/params/arguments/args")
        .cloned()
        .unwrap_or_default();
    // `devices#info`: the resource is the person's devices, the action the part after the `#`.
    match act(
        answers,
        path.rsplit('#').next().unwrap_or_default(),
        &args,
        key,
    ) {
        Ok(result) => succeeded(&id, &result),
        Err(message) => failed(&id, &message),
    }
}

/// The daemon's answer that a call succeeded with `result`.
fn succeeded(id: &Value, result: &Value) -> Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {
        "content": [{"type": "text", "text": ""}],
        "structuredContent": {"status": "ok", "result": result}}})
}

/// The daemon's answer that a call was refused with `message`, as a failed action's is written.
fn failed(id: &Value, message: &str) -> Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {
        "content": [{"type": "text", "text": format!("failed: device-action: {message}")}],
        "isError": true,
        "structuredContent": {"status": "failed",
            "error": {"code": "device-action", "message": message}}}})
}

/// Who the asking account is to this daemon, as its native endpoint answers without a call.
fn who(answers: &Answers, key: &SigningKey) -> Value {
    let home = {
        let held = lock(&answers.home);
        held.as_ref().map(|device| {
            (
                hex::encode(device.cert.user_public_key),
                home_answer(device),
            )
        })
    };
    let mut answer = serde_json::Map::new();
    answer.insert(
        "device".into(),
        serde_json::json!(hex::encode(key.verifying_key().to_bytes())),
    );
    answer.insert("claimed".into(), serde_json::json!(home.is_some()));
    if answers.holds_root {
        answer.insert("holds_root".into(), serde_json::json!(true));
    }
    if let Some((person, home)) = home {
        answer.insert("person".into(), Value::String(person));
        answer.insert("home".into(), home);
    }
    Value::Object(answer)
}

/// What the device action `action` answers with `key`, or why this daemon does not answer it.
fn act(answers: &Answers, action: &str, args: &Value, key: &SigningKey) -> Result<Value, String> {
    match action {
        INFO => Ok(serde_json::json!({
            "device": hex::encode(key.verifying_key().to_bytes()),
        })),
        crate::login::SIGN => {
            let message = args
                .get("message")
                .and_then(Value::as_str)
                .ok_or("the sign action asks no message")?;
            let message = hex::decode(message).map_err(|_| "the message is not hex".to_owned())?;
            Ok(serde_json::json!({
                "signature": hex::encode(key.sign(&message).to_bytes()),
            }))
        }
        crate::login::FOUND if answers.login.is_some() => found(answers, key),
        crate::login::LOG_IN if answers.login.is_some() => log_in(answers, args, key),
        _ => Err("this machine's daemon answers no such device action".to_owned()),
    }
}

/// What a real daemon makes of a computer that is nobody's device yet: a person of its own, whose
/// root certifies its key. It is not filed: the login that follows files the certificate this
/// computer keeps.
fn found(answers: &Answers, key: &SigningKey) -> Result<Value, String> {
    let root = SigningKey::from_bytes(&FOUNDED);
    let device = Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://tilo.test/".into(),
        workspace: "a5b4c3d2-e1f0-4b5a-8796-0f1e2d3c6978".into(),
        workspace_name: None,
        agent: "Tilo".into(),
        cert: cert_for(&root, key, "this computer")?,
        server_cert: cert_for(&root, &root, "this computer's person")?,
        server_address: None,
    };
    *lock(&answers.home) = Some(device);
    Ok(serde_json::json!({ "ok": true }))
}

/// What a real daemon makes of the computer that asked for a login: the certificate the person's
/// root signs for its key, filed, and the device it enters as.
fn log_in(answers: &Answers, args: &Value, key: &SigningKey) -> Result<Value, String> {
    let ticket = args.get("ticket").and_then(Value::as_str);
    if ticket.is_none_or(|ticket| !ticket.starts_with("tilo1")) {
        return Err("the login carries no code".into());
    }
    let agent = args.get("agent").and_then(Value::as_str);
    if agent.is_none_or(str::is_empty) {
        return Err("the login names no agent".into());
    }
    let Some(answer) = answers.login else {
        return Err("this machine's daemon answers no such device action".into());
    };
    match answer {
        Login::Refuse(message) | Login::PendingRefused(message) => return Err(message.to_owned()),
        Login::Wait => std::thread::sleep(std::time::Duration::from_millis(750)),
        Login::Certify | Login::Pending | Login::PendingForever => {}
    }
    let root = SigningKey::from_bytes(&PRINTED);
    let label = args
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !label.is_empty())
        .unwrap_or("the kitchen computer");
    let cert = cert_for(&root, key, label)?;
    let server_cert = cert_for(&root, &root, "the server")?;
    let Some(place) = &answers.place else {
        return Err("this machine's daemon keeps no login".into());
    };
    file_certificate(place, &cert).map_err(|error| error.to_string())?;
    *lock(&answers.home) = Some(Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: WORKSPACE.into(),
        workspace_name: Some("Studio".into()),
        agent: "Tilo".into(),
        cert,
        server_cert,
        server_address: None,
    });
    Ok(serde_json::json!({ "ok": true }))
}

/// A member certificate `root` signs for `key`, issued a minute before now.
fn cert_for(root: &SigningKey, key: &SigningKey, label: &str) -> Result<DeviceCert, String> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .unwrap_or_default();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now_ns.saturating_sub(60_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).map_err(|error| error.to_string())?;
    Ok(cert)
}

/// What the daemon keeps, taken even after a panic elsewhere poisoned the lock.
fn lock<T>(kept: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    kept.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What this daemon answers, as the daemon's clients read its answers.
#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon of a machine answers what the device is and signs with the key it took, and
    /// refuses an action it does not serve, as the daemon's clients read those answers.
    #[test]
    fn a_machines_daemon_answers_what_the_device_is_and_signs_with_its_key() {
        // This machine's own home and runtime directory: the endpoint is not another test's.
        let machine = tilo_test_env::TestHome::new().unwrap();
        let paths = IdentityPaths::under(machine.root().join("device"));
        let daemon = DeviceDaemon::start(&paths).unwrap();
        let actions = daemon.actions();
        assert_eq!(actions.device_key().unwrap(), daemon.device_key());
        let signature = actions.sign(b"a message").unwrap();
        ed25519_dalek::Verifier::verify(
            &ed25519_dalek::VerifyingKey::from_bytes(&daemon.device_key()).unwrap(),
            b"a message",
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .unwrap();
        assert!(actions.ask("sealVault", serde_json::json!({})).is_err());
    }
}
