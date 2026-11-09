//! `tilo login <code>` and `tilo mcp`: this computer as one of the person's
//! devices, for the agent that runs here.
//!
//! The code is what the person's browser tab copied ("Set up with your
//! agent"): a pairing link to that tab. Logging in makes a device key here,
//! asks the workspace's host to have the person's root (which only the tab
//! holds) certify it, and keeps the key and its certificate in this
//! account's state directory. No root is ever here. The device then enters
//! the workspace the code names, as the person, and the tab shows the agent
//! present there with what it does.
//!
//! `tilo mcp` is the agent's MCP server over stdio, under the name `tilo`:
//! it acts as that device, keeps the agent present in the workspace while it
//! runs, and shows every tool call there as one of the agent's actions.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_browser_peer::login::{self as peer_login, Door, SignIn, action, say_here};
use tilo_browser_peer::pairing_link::PairingLink;
use tilo_browser_peer::{HttpClient, HttpFuture, PeerHandle};
use tilo_relay_protocol::web::HereAction;

/// The longest body read from a host or a site.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// How many of the agent's actions the workspace shows at once.
const SHOWN_ACTIONS: usize = 8;
/// The MCP protocol version this server speaks.
const PROTOCOL: &str = "2025-06-18";

/// Where this account keeps the device: `$XDG_STATE_HOME/tilo/device`.
fn device_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_DEVICE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(tilo_paths::state_dir()
        .context("this account has no state directory (no HOME)")?
        .join("tilo/device"))
}

/// What a login keeps beside the device key: public material only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The site whose tab copied the code (`https://tilo.sh/`).
    pub site: String,
    /// The workspace's host (`https://eu1.tilo.sh`).
    pub host_origin: String,
    /// The workspace the device entered.
    pub workspace: String,
    /// The person, hex user root.
    pub user: String,
    /// The agent that runs here.
    pub agent: String,
    /// This device's label, as the person's devices list it.
    pub label: String,
    /// This device's certificate, postcard as hex.
    pub cert: String,
    /// The host's certificate, pinned, postcard as hex.
    pub host_cert: String,
}

impl Device {
    fn of(signed: &SignIn) -> Result<Self> {
        Ok(Self {
            site: signed.link.site.clone(),
            host_origin: signed.link.host_origin.clone(),
            workspace: signed.link.workspace_id(),
            user: hex::encode(signed.link.user_public_key),
            agent: signed.agent.clone(),
            label: signed.label.clone(),
            cert: hex::encode(postcard::to_allocvec(&signed.cert)?),
            host_cert: hex::encode(postcard::to_allocvec(&signed.host_cert)?),
        })
    }

    fn certs(
        &self,
    ) -> Result<(
        tilo_user_identity::DeviceCert,
        tilo_user_identity::DeviceCert,
    )> {
        Ok((
            postcard::from_bytes(&hex::decode(&self.cert)?)?,
            postcard::from_bytes(&hex::decode(&self.host_cert)?)?,
        ))
    }
}

/// Keep `key` and `device` in `dir`: the key readable by this account only,
/// both replaced in one rename each.
pub fn store(dir: &std::path::Path, key: &SigningKey, device: &Device) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let write = |name: &str, bytes: &[u8]| -> Result<()> {
        let staged = dir.join(format!(".{name}.new"));
        let _ = std::fs::remove_file(&staged);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&staged, dir.join(name))?;
        Ok(())
    };
    write("key", &key.to_bytes())?;
    write("device.json", &serde_json::to_vec_pretty(device)?)?;
    Ok(())
}

/// The device kept in `dir`, if this computer logged in.
pub fn load(dir: &std::path::Path) -> Result<Option<(SigningKey, Device)>> {
    use std::os::unix::fs::PermissionsExt as _;
    let key_path = dir.join("key");
    if !key_path.exists() {
        return Ok(None);
    }
    if std::fs::metadata(&key_path)?.permissions().mode() & 0o077 != 0 {
        bail!(
            "{} is readable by other accounts; log in again",
            key_path.display()
        );
    }
    let bytes: [u8; 32] = std::fs::read(&key_path)?
        .try_into()
        .map_err(|_| anyhow!("{} is damaged; log in again", key_path.display()))?;
    let device: Device = serde_json::from_slice(&std::fs::read(dir.join("device.json"))?)?;
    Ok(Some((SigningKey::from_bytes(&bytes), device)))
}

/// Whether this computer logged in (`tilo login`).
pub fn logged_in() -> bool {
    device_dir().is_ok_and(|dir| dir.join("key").exists())
}

/// HTTPS to one origin, as a host's and a site's routes take it.
struct Web {
    client: tilo_net::Client,
    origin: String,
}

impl Web {
    fn new(origin: &str) -> Self {
        Self {
            client: tilo_net::Client::new(),
            origin: origin.trim_end_matches('/').to_owned(),
        }
    }

    async fn get(&self, path: &str) -> Result<(u16, Vec<u8>)> {
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .send()
            .await?;
        let status = response.status().as_u16();
        Ok((status, response.bytes_limited(MAX_BODY).await?.to_vec()))
    }
}

impl HttpClient for Web {
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        let url = format!("{}{path}", self.origin);
        Box::pin(async move {
            let response = self
                .client
                .post(url)
                .header("content-type", "application/octet-stream")
                .body(body)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .bytes_limited(MAX_BODY)
                .await
                .map_err(|error| error.to_string())?;
            Ok((status, body.to_vec()))
        })
    }
}

/// The site's door to the workspace the code names, as its `entry.json`
/// says (`home.admission`), when it names one for that host.
async fn door_of(site: &str, host_origin: &str) -> Option<(Web, String)> {
    let (status, body) = Web::new(site).get("/entry.json").await.ok()?;
    if status != 200 {
        return None;
    }
    let entry: Value = serde_json::from_slice(&body).ok()?;
    let home = entry.get("home")?;
    let home_host = home.pointer("/host/origin")?.as_str()?;
    if home_host.trim_end_matches('/') != host_origin.trim_end_matches('/') {
        return None;
    }
    let admission = tilo_net::Url::parse(home.get("admission")?.as_str()?).ok()?;
    if admission.scheme() != "https" {
        return None;
    }
    let origin = admission.origin().ascii_serialization();
    let path = format!("{}join", admission.path());
    Some((Web::new(&origin), path))
}

/// This computer's name, as a device label says it.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "this computer".to_owned())
}

/// `"Claude Code on build-box"`, at most 64 characters.
#[must_use]
pub fn label(agent: &str, host: &str) -> String {
    format!("{agent} on {host}").chars().take(64).collect()
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

/// Enter the workspace `device` names, as that device.
async fn connect(key: &SigningKey, device: &Device) -> Result<(PeerHandle, u64)> {
    let link = PairingLink::parse(&format!(
        "{}#/@{}/w/{}?pair={}",
        device.site,
        device.host_origin.trim_start_matches("https://"),
        device.workspace,
        device.user
    ))
    .map_err(|error| anyhow!(error))?;
    let host_web = Web::new(&device.host_origin);
    let info = tilo_browser_peer::host_info(&host_web).await?;
    let address = info
        .address
        .context("the workspace's host takes no device connections")?;
    let endpoint = match address
        .relay_url
        .as_deref()
        .and_then(|url| url.parse().ok())
    {
        Some(relay) => address.endpoint_addr.with_relay_url(relay),
        None => address.endpoint_addr,
    };
    let (cert, host_cert) = device.certs()?;
    let signed = SignIn {
        link,
        agent: device.agent.clone(),
        label: device.label.clone(),
        host: tilo_browser_peer::Host::at(endpoint),
        cert,
        host_cert,
    };
    let door = door_of(&device.site, &device.host_origin).await;
    let door = door.as_ref().map(|(client, path)| Door {
        client,
        path: path.clone(),
    });
    let peer = peer_login::enter(&host_web, door.as_ref(), &signed, Arc::new(key.clone())).await?;
    let workspace_id = tilo_relay_protocol::room_workspace_id(&device.workspace)?;
    Ok((peer, workspace_id))
}

/// `tilo login <code> [--agent NAME]`.
pub fn login(code: &str, agent: &str) -> Result<ExitCode> {
    let link = PairingLink::parse(code.trim()).map_err(|error| anyhow!(error))?;
    let agent = agent.trim();
    if agent.is_empty() {
        bail!("--agent needs your name, like --agent \"Claude Code\"");
    }
    let label = label(agent, &hostname());
    let dir = device_dir()?;
    let key = SigningKey::from_bytes(&rand::random());
    eprintln!("Logging in as a device of the person whose tab copied this code…");
    let runtime = runtime()?;
    let signed = runtime.block_on(async {
        let host_web = Web::new(&link.host_origin);
        let signed = peer_login::sign_in(&host_web, &link, agent, &label, &key, |code| {
            eprintln!("Pairing code {code}. The tab that copied the code approves it by itself; nothing to do there.");
        })
        .await
        .map_err(|error| match error {
            tilo_browser_peer::CertifyError::Expired => anyhow!(
                "the tab did not approve this computer in time: the code works once, for 10 minutes, and only while that tab is open. Copy a new one there."
            ),
            tilo_browser_peer::CertifyError::Denied(reason) if reason.contains("declined") => {
                anyhow!("the tab declined this computer. Copy a new code there to try again.")
            }
            other => anyhow!("logging in failed: {other}"),
        })?;
        anyhow::Ok(signed)
    })?;
    let device = Device::of(&signed)?;
    store(&dir, &key, &device)?;
    let days = signed.cert.expires_at_ns.map_or(0, |end| {
        end.saturating_sub(signed.cert.issued_at_ns) / (24 * 3600 * 1_000_000_000)
    });
    println!(
        "Logged in: this computer is \"{label}\", a device of yours for {days} days. Your browser keeps your identity; nothing secret of yours is stored here but this device's own key."
    );
    match runtime.block_on(connect(&key, &device)) {
        Ok((peer, _)) => {
            println!(
                "Entered your workspace on {}: your tab shows {agent} there.",
                host_of(&device.host_origin)
            );
            peer.close();
        }
        Err(error) => println!(
            "Logged in, but entering your workspace failed for now ({error:#}); `tilo mcp` tries again."
        ),
    }
    println!();
    println!("Next, give your agent Tilo's tools, the MCP server named tilo:");
    println!("  Claude Code:   tilo agent connect claude");
    println!("  Codex:         tilo agent connect codex");
    println!("  Other agents:  add the stdio MCP server `tilo mcp`, named tilo");
    println!("Then call its tilo_status tool to check that you can act.");
    Ok(ExitCode::SUCCESS)
}

fn host_of(origin: &str) -> &str {
    origin.trim_start_matches("https://").trim_end_matches('/')
}

/// What the MCP server holds while it runs.
struct Serving {
    device: Option<Device>,
    peer: Option<(PeerHandle, u64)>,
    problem: Option<String>,
    shown: Vec<HereAction>,
}

/// `tilo mcp`: the MCP server over stdio.
pub fn mcp() -> Result<ExitCode> {
    let runtime = runtime()?;
    let stored = load(&device_dir()?)?;
    let serving = Arc::new(Mutex::new(Serving {
        device: None,
        peer: None,
        problem: None,
        shown: Vec::new(),
    }));
    if let Some((key, device)) = stored {
        serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .device = Some(device.clone());
        let held = serving.clone();
        runtime.spawn(async move {
            let connected = connect(&key, &device).await;
            let mut serving = held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match connected {
                Ok(peer) => serving.peer = Some(peer),
                Err(error) => serving.problem = Some(format!("{error:#}")),
            }
        });
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(answer) = runtime.block_on(answer(&serving, &line)) else {
            continue;
        };
        writeln!(stdout, "{answer}")?;
        stdout.flush()?;
    }
    let held = serving
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .peer
        .take();
    if let Some((peer, _)) = held {
        peer.close();
    }
    Ok(ExitCode::SUCCESS)
}

const NOT_LOGGED_IN: &str = "This computer is not logged in to Tilo. Ask the person to open Tilo in their browser, choose \"Set up with your agent\" and give you the code it copies; then run: tilo login '<code>' --agent \"<your name>\"";

fn tools() -> Value {
    json!([
        {"name": "tilo_status", "description": "Who you act as in Tilo: the person, this device, the workspace and whether you are in it.",
         "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}},
        {"name": "tilo_show", "description": "Tell the person what you are working on. It appears beside your name in their Tilo workspace.",
         "inputSchema": {"type": "object", "properties": {"doing": {"type": "string", "description": "What you are doing, in a few words"}},
                         "required": ["doing"], "additionalProperties": false}},
    ])
}

/// The answer to one JSON-RPC line, or `None` for a notification.
async fn answer(serving: &Mutex<Serving>, line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => return Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": error.to_string()}}).to_string()),
    };
    let id = request.get("id").cloned()?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => Ok(
            json!({"protocolVersion": PROTOCOL, "capabilities": {"tools": {}},
            "serverInfo": {"name": "tilo", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Tilo is the person's workspace. Call tilo_status first; use tilo_show to tell the person what you are doing."}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => Ok(call(serving, request.get("params").cloned().unwrap_or_default()).await),
        _ => Err(json!({"code": -32601, "message": format!("no method {method}")})),
    };
    Some(
        match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        }
        .to_string(),
    )
}

fn said(text: &str, error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": error})
}

async fn call(serving: &Mutex<Serving>, params: Value) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let arguments = params.get("arguments").cloned().unwrap_or_default();
    let (device, peer, problem) = {
        let serving = serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            serving.device.clone(),
            serving.peer.clone(),
            serving.problem.clone(),
        )
    };
    let Some(device) = device else {
        return said(NOT_LOGGED_IN, true);
    };
    let (title, resource, reply) = match name.as_str() {
        "tilo_status" => {
            let state = match (&peer, &problem) {
                (Some(_), _) => "in the workspace now".to_owned(),
                (None, Some(problem)) => format!("not in the workspace: {problem}"),
                (None, None) => "entering the workspace…".to_owned(),
            };
            let user: Vec<char> = device.user.chars().collect();
            let first: String = user.iter().take(8).collect();
            let last: String = user.iter().skip(user.len().saturating_sub(8)).collect();
            let text = format!(
                "You act in Tilo as the person {first}…{last} on the device \"{}\", in workspace {} on {}: {state}. The person sees you there as {} with what you do.",
                device.label,
                device.workspace,
                host_of(&device.host_origin),
                device.agent,
            );
            (
                "Checked it can act".to_owned(),
                String::new(),
                said(&text, false),
            )
        }
        "tilo_show" => {
            let doing = arguments
                .get("doing")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            if doing.is_empty() {
                return said(
                    "tilo_show needs `doing`: what you are working on, in a few words.",
                    true,
                );
            }
            let text = if peer.is_some() {
                format!("The person now sees: {} — {doing}", device.agent)
            } else {
                "Not in the workspace yet; call tilo_status.".to_owned()
            };
            (
                doing.as_str().chars().take(80).collect::<String>(),
                String::new(),
                said(&text, peer.is_none()),
            )
        }
        other => {
            return said(
                &format!("Tilo has no tool {other}; tools/list names them."),
                true,
            );
        }
    };
    if let Some((peer, workspace_id)) = peer {
        let actions = {
            let mut serving = serving
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            serving.shown.push(action(&title, &resource, "Tilo MCP"));
            let excess = serving.shown.len().saturating_sub(SHOWN_ACTIONS);
            serving.shown.drain(..excess);
            serving.shown.clone()
        };
        let _ = say_here(&peer, workspace_id, &device.agent, actions).await;
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        Device {
            site: "https://tilo.sh/".into(),
            host_origin: "https://eu1.tilo.sh".into(),
            workspace: "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0".into(),
            user: "ab".repeat(32),
            agent: "Claude Code".into(),
            label: "Claude Code on build-box".into(),
            cert: "00".into(),
            host_cert: "00".into(),
        }
    }

    #[test]
    fn the_device_is_kept_for_this_account_only_and_read_back() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let place = dir.path().join("device");
        let key = SigningKey::from_bytes(&[7; 32]);
        store(&place, &key, &device()).unwrap();
        assert_eq!(
            std::fs::metadata(place.join("key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&place).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let (read, kept) = load(&place).unwrap().unwrap();
        assert_eq!(read.to_bytes(), key.to_bytes());
        assert_eq!(kept, device());
        // A key others can read is not used.
        std::fs::set_permissions(place.join("key"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(load(&place).is_err());
        assert!(load(&dir.path().join("none")).unwrap().is_none());
    }

    #[test]
    fn a_label_names_the_agent_and_the_computer_within_bounds() {
        assert_eq!(
            label("Claude Code", "build-box"),
            "Claude Code on build-box"
        );
        assert_eq!(label(&"a".repeat(100), "x").chars().count(), 64);
    }

    #[tokio::test]
    async fn the_mcp_server_lists_its_tools_and_says_how_to_log_in_without_a_device() {
        let serving = Mutex::new(Serving {
            device: None,
            peer: None,
            problem: None,
            shown: Vec::new(),
        });
        let init: Value = serde_json::from_str(
            &answer(
                &serving,
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "tilo");
        assert!(
            answer(
                &serving,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .await
            .is_none()
        );
        let listed: Value = serde_json::from_str(
            &answer(
                &serving,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["tilo_status", "tilo_show"]);
        let called: Value = serde_json::from_str(
            &answer(
                &serving,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_status"}}"#,
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(called["result"]["isError"], true);
        assert!(
            called["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("tilo login")
        );
    }

    #[tokio::test]
    async fn status_says_who_the_agent_acts_as() {
        let serving = Mutex::new(Serving {
            device: Some(device()),
            peer: None,
            problem: Some("offline".into()),
            shown: Vec::new(),
        });
        let called: Value = serde_json::from_str(
            &answer(
                &serving,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_status"}}"#,
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let text = called["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("Claude Code on build-box")
                && text.contains("eu1.tilo.sh")
                && text.contains("offline"),
            "{text}"
        );
    }
}
