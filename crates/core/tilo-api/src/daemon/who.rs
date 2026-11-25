//! What this computer's daemon says about the device it is, at its native endpoint: who an
//! account is to it (`account#who`), logging the device in with a ticket (`devices#logIn`), and
//! certifying the key another person signed in on the computer acts as (`seats#certified`).
//!
//! The device and its person are its certificates'; the server it enters its person's workspaces
//! through is the daemon's to know, from its copy of the person's own workspace. Nothing of either
//! is kept in a file a program reads beside the daemon: every program of the computer asks it here.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_device_identity::login::{Device, ServerAddress};

/// What the native endpoint is asked for who the asking account is.
pub const WHO: &str = "account#who";

/// How long a question at the native endpoint may take to be answered.
const ASKING: Duration = Duration::from_secs(10);
/// The longest answer read.
const ANSWER_MAX: u64 = 1 << 20;
/// How long a login with a ticket is waited for, the person approving it on another device
/// among it.
const LOGGING_IN: Duration = Duration::from_secs(15 * 60);
/// How long one call of the login waits at the daemon before it is asked about again.
const WAIT_MS: u64 = 55_000;
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
}

/// The `home` of an `account#who` answer: where this device enters, as `device` the daemon serves.
#[must_use]
pub fn home_answer(device: &Device) -> Value {
    json!({
        "site": device.site,
        "host_origin": device.host_origin,
        "workspace": device.workspace,
        "workspace_name": device.workspace_name,
        "agent": device.agent,
        "cert": canonical_hex(&device.cert),
        "server_cert": canonical_hex(&device.server_cert),
        "server_address": device.server_address,
    })
}

fn canonical_hex(cert: &tilo_user_identity::DeviceCert) -> Option<String> {
    tilo_user_identity::canonical::canonical_bytes(cert)
        .ok()
        .map(hex::encode)
}

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
    let answer = ask(&json!({"native": WHO}))?;
    let key = |name: &str| -> Option<[u8; 32]> {
        hex::decode(answer.get(name)?.as_str()?)
            .ok()?
            .try_into()
            .ok()
    };
    Ok(Who {
        person: key("person"),
        device: key("device").ok_or("the daemon named no device key")?,
        label: answer
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned),
        claimed: answer.get("claimed").and_then(Value::as_bool) == Some(true),
        holds_root: answer.get("holds_root").and_then(Value::as_bool) == Some(true),
        home: answer.get("home").and_then(home_of),
        certify_person: answer
            .get("certify")
            .and_then(|certify| certify.get("person"))
            .and_then(Value::as_str)
            .and_then(|person| hex::decode(person).ok())
            .and_then(|person| <[u8; 32]>::try_from(person).ok()),
    })
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
    logging_in(&json!({"ticket": ticket, "agent": agent, "label": label}))
}

/// Log this computer, which holds its person's root, in at the server whose line (what it keeps
/// in `web-host.json`) is `line`, through its daemon (`devices#logIn`): the daemon names the
/// server among the hosts of the person's own workspace and its person's part starts again
/// through it. Who this account is to the daemon afterwards.
///
/// # Errors
/// No daemon answers, or the login did not work, saying why.
pub fn log_in_at(line: &str) -> Result<Who, String> {
    logging_in(&json!({"server": line}))
}

/// `devices#logIn` with `args`, followed until the daemon's person's part serves what it made.
fn logging_in(args: &Value) -> Result<Who, String> {
    let place = super::this_computer()?;
    let started = std::time::Instant::now();
    let before = who()?.home;
    let mut stream = connect(&place, LOGGING_IN)?;
    let mut lines = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| error.to_string())?
            .take(ANSWER_MAX),
    );
    let mut id: u64 = 0;
    let mut arguments = json!({"path": "devices#logIn", "wait_ms": WAIT_MS, "args": args});
    let mut tool = "tilo_do";
    loop {
        id = id.saturating_add(1);
        let request = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}});
        writeln!(stream, "{request}").map_err(|error| error.to_string())?;
        let mut line = String::new();
        lines
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let answer: Value = serde_json::from_str(&line)
            .map_err(|_| "this computer's daemon ended while it logged in".to_owned())?;
        if let Some(error) = answer.pointer("/error/message").and_then(Value::as_str) {
            return Err(error.to_owned());
        }
        let text = answer
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if answer.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
            // `failed: <code>: <why>`, of which the person reads why.
            let said = text.strip_prefix("failed: ").map_or(text, |rest| {
                rest.split_once(": ").map_or(rest, |(_, why)| why)
            });
            return Err(said.to_owned());
        }
        let Some(pending) = text.strip_prefix("pending ") else {
            break;
        };
        if started.elapsed() > LOGGING_IN {
            return Err("your device did not answer in time: ask for a new ticket".to_owned());
        }
        let invocation = pending.split_once(':').map_or(pending, |(id, _)| id).trim();
        arguments = json!({"invocation_id": invocation, "wait_ms": WAIT_MS});
        tool = "tilo_status";
    }
    // The connection ends here, its reading half too, so the daemon hears that it ended.
    drop(lines);
    drop(stream);
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

/// Whether this account signed in on this computer with no person yet, and may make one here
/// ([`crate::daemon::DevicePlace::found`]): the key the daemon made it waits to be certified
/// once it does.
///
/// # Errors
/// No daemon answers.
pub fn founding() -> Result<bool, String> {
    let answer = ask(&json!({"native": WHO}))?;
    Ok(answer.get("founding").and_then(Value::as_bool) == Some(true))
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
    if ask(&json!({"native": WHO}))?.get("certify").is_none() {
        return Ok(false);
    }
    let answer = ask(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": "seats#certified", "args": {}},
        }}),
    )?;
    Ok(refused(&answer)?.is_some())
}

/// Whether `answer` to a `seats#certified` call certified a key, refusing with its own words
/// when it did not.
fn refused(answer: &Value) -> Result<Option<String>, String> {
    if let Some(error) = answer.pointer("/error/message").and_then(Value::as_str) {
        return Err(error.to_owned());
    }
    if answer.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
        let said = answer
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("the daemon refused the certificate");
        return Err(said.to_owned());
    }
    Ok(answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// [`certify_my_key`], at the daemon whose native endpoint is `socket`.
///
/// # Errors
/// As [`certify_my_key`].
pub fn certify_key_at(
    socket: &std::path::Path,
    signer: &dyn tilo_user_identity::StatementSigner,
) -> Result<bool, String> {
    let asked = ask_at(socket, &json!({"native": WHO}))?;
    let Some(certify) = asked.get("certify") else {
        return Ok(false);
    };
    let key = |name: &str| -> Option<[u8; 32]> {
        hex::decode(certify.get(name)?.as_str()?)
            .ok()?
            .try_into()
            .ok()
    };
    let (Some(person), Some(device)) = (key("person"), key("key")) else {
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
    let answer = ask_at(
        socket,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": "seats#certified", "args": {"cert": cert}},
        }}),
    )?;
    if let Some(error) = answer.pointer("/error/message").and_then(Value::as_str) {
        return Err(error.to_owned());
    }
    if answer.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
        let said = answer
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("the daemon refused the certificate");
        return Err(said.to_owned());
    }
    Ok(true)
}

/// Ask `request` at this computer's native endpoint, one line each way.
fn ask(request: &Value) -> Result<Value, String> {
    ask_at(&super::this_computer()?.native(), request)
}

/// Ask `request` at the native endpoint `socket`, one line each way.
fn ask_at(socket: &std::path::Path, request: &Value) -> Result<Value, String> {
    let mut stream = connect_to(socket, ASKING)?;
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream.take(ANSWER_MAX))
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&line).map_err(|error| format!("this computer's daemon answered: {error}"))
}

/// A connection to the native endpoint of the daemon at `place`, answering within `within`.
fn connect(
    place: &super::DaemonPlace,
    within: Duration,
) -> Result<tilo_files::native::socket::Stream, String> {
    connect_to(&place.native(), within)
}

/// A connection to the native endpoint `socket`, answering within `within`.
fn connect_to(
    socket: &std::path::Path,
    within: Duration,
) -> Result<tilo_files::native::socket::Stream, String> {
    let stream = tilo_files::native::socket::connect(socket).map_err(|error| {
        format!(
            "Tilo isn't running on this computer ({}: {error})",
            socket.display(),
        )
    })?;
    stream
        .set_read_timeout(Some(within))
        .map_err(|error| error.to_string())?;
    Ok(stream)
}
