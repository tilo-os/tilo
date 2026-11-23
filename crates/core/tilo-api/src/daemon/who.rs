//! What this computer's daemon says about the device it is, at its native endpoint: who an
//! account is to it (`account#who`), and logging the device in with a ticket (`devices#logIn`).
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
    /// Whether a person's certificate names this device.
    pub claimed: bool,
    /// The device this computer is as it enters its person's workspaces, once the daemon knows
    /// their server.
    pub home: Option<Device>,
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
        home: answer.get("home").and_then(home_of),
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

/// This computer's device key, with the device it is as it enters its person's workspaces, as its
/// daemon knows it: `None` while the daemon knows no server of theirs, or none answers. Reading
/// the key makes this process undumpable; [`home`] reads what needs no key.
///
/// # Errors
/// The device key cannot be read, or the daemon serves another key than this account's.
pub fn here() -> anyhow::Result<Option<(ed25519_dalek::SigningKey, Device)>> {
    let Ok(Some(device)) = home() else {
        return Ok(None);
    };
    let key = tilo_device_identity::persist::load_device_key(&crate::calls::device_keys())
        .map_err(|_| anyhow::anyhow!(tilo_device_identity::login::LOGIN_DAMAGED))?;
    if device.cert.device_public_key != key.verifying_key().to_bytes() {
        anyhow::bail!(
            "this computer's daemon serves another device key than this account's: log in again \
             with a ticket `tilo device add` prints on one of your devices"
        );
    }
    Ok(Some((key, device)))
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
        let now = who()?;
        match &now.home {
            Some(home) if stopped || Some(home) != before.as_ref() => return Ok(now),
            Some(_) => {}
            None => stopped = true,
        }
        if restarted.elapsed() > RESTARTING {
            return Err(format!(
                "this computer is one of your devices now, but Tilo did not start again as it in \
                 time: {}",
                super::service::LOOK
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Ask `request` at this computer's native endpoint, one line each way.
fn ask(request: &Value) -> Result<Value, String> {
    let place = super::this_computer()?;
    let mut stream = connect(&place, ASKING)?;
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
) -> Result<std::os::unix::net::UnixStream, String> {
    let stream = tilo_files::native::socket::connect(&place.native()).map_err(|error| {
        format!(
            "Tilo isn't running on this computer ({}: {error})",
            place.native().display(),
        )
    })?;
    stream
        .set_read_timeout(Some(within))
        .map_err(|error| error.to_string())?;
    Ok(stream)
}
