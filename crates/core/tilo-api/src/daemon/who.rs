//! What this computer's daemon says about the device it is, at its native endpoint: who an
//! account is to it (`account#who`), logging the device in with a ticket (`devices#logIn`), and
//! certifying the key another person signed in on the computer acts as (`seats#certified`).
//!
//! The device and its person are its certificates'; the server it enters its person's workspaces
//! through is the daemon's to know, from its copy of the person's own workspace. Nothing of either
//! is kept in a file a program reads beside the daemon: every program of the computer asks it here.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_device_identity::login::daemon::{DeviceActions, canonical_hex};
use tilo_device_identity::login::{Device, LOG_IN, ServerAddress};

/// What the native endpoint is asked for who the asking account is.
pub const WHO: &str = "account#who";

/// The longest answer read.
const ANSWER_MAX: u64 = 1 << 20;
/// How long the daemon's person's part is waited for to start again as the device a login made.
const RESTARTING: Duration = Duration::from_secs(60);

/// Who an account is to this computer's daemon.
#[derive(Clone, Debug)]
pub struct Who {
    /// The person it acts as, when there is one.
    pub person: Option<[u8; 32]>,
    /// This computer's device key.
    pub device: [u8; 32],
    /// How the person's devices name it, once a certificate does.
    pub label: Option<String>,
    /// Whether this computer's identity is one its person's services can start as: a person's
    /// certificate names this device and the account's identity store names that person.
    pub claimed: bool,
    /// Whether this computer keeps the person's root: what their backup and recovery words are
    /// made from.
    pub holds_root: bool,
    /// The device this computer is as it enters its person's workspaces, once the daemon knows
    /// their server.
    pub home: Option<Device>,
    /// The person this account acts as here once it certifies the key the daemon made it, while
    /// it waits to: the device's own person has none.
    pub certify_person: Option<[u8; 32]>,
    /// Whether this account may make this computer's person ([`super::DevicePlace::found`]).
    pub founding: bool,
}

/// The `home` of an `account#who` answer: where this device enters, as `device` the daemon serves.
pub use tilo_device_identity::login::daemon::home_answer;

/// The device a `home` answer names.
fn home_of(home: &Value) -> Option<Device> {
    let text = |name: &str| home.get(name).and_then(Value::as_str).map(str::to_owned);
    let cert = |name: &str| {
        let bytes = hex::decode(home.get(name)?.as_str()?).ok()?;
        tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok()
    };
    Some(Device {
        site: text("site")?,
        host_origin: text("host_origin")?,
        workspace: text("workspace")?,
        workspace_name: text("workspace_name"),
        agent: text("agent").unwrap_or_default(),
        cert: cert("cert")?,
        server_cert: cert("server_cert")?,
        server_address: home
            .get("server_address")
            .cloned()
            .and_then(|address| serde_json::from_value::<Option<ServerAddress>>(address).ok())
            .flatten(),
    })
}

/// Who this account is to this computer's daemon, asked at its native endpoint.
///
/// # Errors
/// No daemon answers there, or its answer is no `account#who`.
pub fn who() -> Result<Who, String> {
    let answer = ask_at(&super::this_computer()?.native(), &json!({"native": WHO}))?;
    Ok(Who {
        person: key_of(&answer, "person"),
        device: key_of(&answer, "device").ok_or("the daemon named no device key")?,
        label: answer
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned),
        claimed: answer.get("claimed").and_then(Value::as_bool) == Some(true),
        holds_root: answer.get("holds_root").and_then(Value::as_bool) == Some(true),
        home: answer.get("home").and_then(home_of),
        certify_person: answer
            .get("certify")
            .and_then(|certify| key_of(certify, "person")),
        founding: answer.get("founding").and_then(Value::as_bool) == Some(true),
    })
}

/// The 32 bytes the hex field `name` of `answer` names.
fn key_of(answer: &Value, name: &str) -> Option<[u8; 32]> {
    hex::decode(answer.get(name)?.as_str()?)
        .ok()?
        .try_into()
        .ok()
}

/// The device this computer is as it enters its person's workspaces, as its daemon knows it:
/// `None` while the daemon knows no server of theirs.
///
/// # Errors
/// No daemon answers.
pub fn home() -> Result<Option<Device>, String> {
    who().map(|who| who.home)
}

/// Log this computer in with `ticket` through its daemon (`devices#logIn`), for `agent`, listed
/// as `label`: the daemon spends the ticket, files the certificate the person's root signs, and
/// its person's part starts again as the device it became. Who this account is to the daemon
/// afterwards.
///
/// # Errors
/// No daemon answers, or the login did not work, saying why.
pub fn log_in(ticket: &str, agent: &str, label: &str) -> Result<Who, String> {
    logging_in(json!({"ticket": ticket, "agent": agent, "label": label}))
}

/// Log this computer, which holds its person's root, in at the server whose line (what it keeps
/// in `web-host.json`) is `line`, through its daemon (`devices#logIn`): the daemon names the
/// server among the hosts of the person's own workspace and its person's part starts again
/// through it. Who this account is to the daemon afterwards.
///
/// # Errors
/// No daemon answers, or the login did not work, saying why.
pub fn log_in_at(line: &str) -> Result<Who, String> {
    logging_in(json!({"server": line}))
}

/// `devices#logIn` with `args`, followed until the daemon's person's part serves what it made.
fn logging_in(args: Value) -> Result<Who, String> {
    let before = who()?.home;
    // The call is the person's to answer on another of their devices: the daemon reports the
    // stage it is in, and answers what it made once they have.
    super::this_computer()?.act_followed(LOG_IN, args)?;
    // The person's part starts again as the device the login made: answered once it serves it,
    // which is another than before, or the same once it was seen to start again.
    let restarted = std::time::Instant::now();
    let mut stopped = false;
    loop {
        // While the person's part starts again the endpoint may take longer than a question
        // waits to answer: that is the restart under way, asked about again until it is late.
        let asked = who();
        match asked.as_ref().map(|now| &now.home) {
            Ok(Some(home)) if stopped || Some(home) != before.as_ref() => return asked,
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => stopped = true,
        }
        if restarted.elapsed() > RESTARTING {
            let why = asked
                .err()
                .map_or_else(String::new, |error| format!(" ({error})"));
            return Err(format!(
                "this computer is one of your devices now, but Tilo did not start again as it in \
                 time{why}: {}",
                super::service::LOOK
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Certify the key this computer's daemon made for this account, when the account signed in as
/// another person than the device's: their identity `signer` certifies it as a member's device,
/// and the account acts as that key from then on. Answers whether there was a key to certify;
/// the device's own person has none.
///
/// # Errors
/// No daemon answers, the key is to be another person's than `signer`'s, or the daemon refused
/// the certificate.
pub fn certify_my_key(signer: &dyn tilo_user_identity::StatementSigner) -> Result<bool, String> {
    certify_key_at(&super::this_computer()?.native(), signer)
}

/// Certify the key this computer's daemon made for this account with the root the daemon keeps
/// for that account alone: the person made their identity on this computer, so its session
/// holds only their public record and cannot sign for them. Answers whether there was a key to
/// certify: the daemon names none for the account it runs as, or for a key already certified,
/// and those never call `seats#certified`, which the person's own endpoint does not serve.
///
/// # Errors
/// No daemon answers, it keeps no root of this person's, or it refused the certificate.
pub fn certify_here() -> Result<bool, String> {
    let socket = super::this_computer()?.native();
    let asked = ask_at(&socket, &json!({"native": WHO}))?;
    certified_at(&socket, &asked, &json!({}))
}

/// Certify the key the daemon at `socket` made this account by calling `seats#certified` with
/// `args`, unless `asked` (its `account#who`) names none: the daemon names none for the account it
/// runs as, or for a key already certified. Answers whether there was a key to certify, or the
/// daemon's words when it refused.
fn certified_at(socket: &Path, asked: &Value, args: &Value) -> Result<bool, String> {
    if asked.get("certify").is_none() {
        return Ok(false);
    }
    let (typed, text, failed) = DeviceActions::at(socket)
        .tool("tilo_do", &json!({"path": "seats#certified", "args": args}))
        .map_err(|refused| refused.message)?;
    if failed {
        return Err(typed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or(&text)
            .to_owned());
    }
    Ok(true)
}

/// [`certify_my_key`], at the daemon whose native endpoint is `socket`.
///
/// # Errors
/// As [`certify_my_key`].
pub fn certify_key_at(
    socket: &Path,
    signer: &dyn tilo_user_identity::StatementSigner,
) -> Result<bool, String> {
    let asked = ask_at(socket, &json!({"native": WHO}))?;
    let Some(certify) = asked.get("certify") else {
        return Ok(false);
    };
    let (Some(person), Some(device)) = (key_of(certify, "person"), key_of(certify, "key")) else {
        return Err("the daemon named no key to certify".to_owned());
    };
    if person != signer.public_key() {
        return Err(
            "this computer signed this account in as another person than its identity".to_owned(),
        );
    }
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person,
        device_public_key: device,
        device_label: tilo_user_identity::UNNAMED_SHARED_DEVICE.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, signer).map_err(|error| error.to_string())?;
    let cert = canonical_hex(&cert).ok_or("the certificate could not be encoded")?;
    certified_at(socket, &asked, &json!({"cert": cert}))
}

/// Ask `request` at the native endpoint `socket`, one line each way.
///
/// No read bound: the daemon answers `account#who` from its device's own state, and a start that
/// is still bringing its person's part up answers it all the same. A question that gave up after
/// a fixed wait read the socket's timeout as the device's answer — "this account's identity was
/// not made on this computer" when the daemon was merely starting — and left the person with no
/// session. The wait is the daemon's to give, as it is for the shell's link.
fn ask_at(socket: &Path, request: &Value) -> Result<Value, String> {
    let mut stream = tilo_files::native::socket::connect(socket).map_err(|error| {
        format!(
            "Tilo isn't running on this computer ({}: {error})",
            socket.display(),
        )
    })?;
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream.take(ANSWER_MAX))
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&line).map_err(|error| format!("this computer's daemon answered: {error}"))
}
