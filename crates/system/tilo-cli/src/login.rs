//! `tilo login <ticket>` and `tilo mcp`: this computer as one of the person's devices, for the agent
//! that runs here.
//!
//! The ticket comes from another device of the person's (`tilo device add` on one of their
//! computers, or the prompt "Set up with your agent" copies), and names that device's iroh endpoint
//! and a one-time secret ([`tilo_device_identity::login`]). `tilo login` loads or makes this
//! computer's one device key, dials the issuing device over the device action service
//! (`tilo-actions/1`), and calls the certify action there with the secret and its key. Once the
//! person approves, the answer is the certificate their root signed over this computer's key, with
//! the workspace and the device to enter it through: the login keeps them and enters
//! the workspace as the person, and tells the agent how to add Tilo's MCP server to itself. Nothing is asked of a web site.
//!
//! `tilo mcp` ([`crate::mcp`]) is the agent's MCP server over stdio, under the name `tilo`, acting
//! as that device.

use std::path::Path;
use std::process::ExitCode;
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_device_identity::login::{Direction, REPORTED, Step, Ticket};
use tilo_session_client::Outbox as _;
use tilo_session_client::device::Device as Entered;

use crate::agent::{self, Agent};
use crate::api::Calls;
pub(crate) use tilo_api::daemon::enter::{
    LOGIN_ENDED, NOT_LOGGED_IN, UNREACHED, choose_workspace, connect, debug_why, enter_said,
    host_of, server_of, signed_over_iroh,
};
pub use tilo_device_identity::login::{Device, Place, served_device_key};

/// The site a login names when the device that added this computer names none.
pub const DEFAULT_SITE: &str = "https://tilo.sh";

/// For tests only: PEM files (separated as `PATH` is) whose certificates
/// HTTPS also trusts, each as a CA and as that exact server certificate.
/// Verification stays on; production trust is the system's alone.
pub const TEST_CA_ENV: &str = "TILO_TEST_CA_FILE";

/// What Tilo's tools let the agent do, as it tells the person.
const WHAT_TILO_DOES: &str = "you are in their Tilo workspace as them, where they see you by \
     name with what you do: tilo_ls lists its apps and documents, tilo_read reads one, \
     tilo_do runs their actions, and tilo_do with path terminal#run runs a shell command \
     there, all of which History keeps as your run";

/// The HTTPS client every request here goes through.
static CLIENT: OnceLock<tilo_net::Client> = OnceLock::new();

pub(crate) fn http() -> tilo_net::Client {
    CLIENT.get_or_init(tilo_net::Client::new).clone()
}

/// Trust what [`TEST_CA_ENV`] names besides the system's roots, saying so
/// on stderr, when it is set.
pub(crate) fn trust_test_certificates() -> Result<()> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject as _;
    let Some(files) = std::env::var_os(TEST_CA_ENV).filter(|files| !files.is_empty()) else {
        return Ok(());
    };
    let mut trusted = Vec::new();
    for path in std::env::split_paths(&files) {
        let pem = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let found = CertificateDer::pem_slice_iter(&pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| anyhow!("{} is not PEM certificates: {error}", path.display()))?;
        if found.is_empty() {
            bail!("{} holds no PEM certificate", path.display());
        }
        eprintln!(
            "WARNING: {TEST_CA_ENV} is set, so HTTPS here also trusts {} as a test's own. \
             Never set it outside a test.",
            path.display()
        );
        trusted.extend(found);
    }
    let client = tilo_net::Client::builder()
        .tls(tilo_net::client_config_also_trusting(trusted)?)
        .build()?;
    CLIENT
        .set(client)
        .map_err(|_| anyhow!("HTTPS was set up before {TEST_CA_ENV} was read"))?;
    Ok(())
}

/// Whether a person's certificate names this computer's device key: it is one of their devices,
/// read without the key, so the process stays dumpable.
pub fn logged_in() -> bool {
    served_device_key(&crate::identity::paths()).is_some()
}

/// This computer's device key, with the device it is as it enters its person's workspaces, as its
/// daemon knows it ([`tilo_api::daemon::who::here`]): `None` while it knows no server of theirs.
///
/// # Errors
/// The device key cannot be read, or the daemon serves another.
pub fn here() -> Result<Option<(SigningKey, Device)>> {
    tilo_api::daemon::who::here()
}

/// The device this computer is as it enters its person's workspaces, read without the device key
/// ([`tilo_api::daemon::who::home`]): `None` while its daemon knows no server of theirs, or none
/// runs.
pub fn home() -> Option<Device> {
    tilo_api::daemon::who::home().ok().flatten()
}

pub(crate) fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "this computer".to_owned())
}

/// `"Reviewer on build-box"`, the name this computer asks the server for,
/// which it keeps unless another device of the person reads by it, within
/// what a certificate's label carries.
#[must_use]
pub fn label(agent: &str, host: &str) -> String {
    tilo_device_identity::login::device_label(agent, host)
}

/// What a login that worked says: the device this computer now is, and which of the person's
/// agents runs on it ([`which_agent`]).
fn logged_in_said(label: &str, days: u64, agent: &str) -> String {
    format!(
        "Logged in: this computer is now \"{label}\", one of the person's devices for {days} days. \
         Their identity stays in their browser; only this device's own key is kept here.\n{}",
        which_agent(agent)
    )
}

/// Which of the person's agents a login made this computer's agent: every computer of theirs
/// that logs in under the same name is that one agent, with its name, colour and history in their
/// workspace; another name is another agent.
fn which_agent(name: &str) -> String {
    format!(
        "You work in Tilo as the person's agent \"{name}\": they see you by that name, and any \
         computer of theirs logged in as \"{name}\" is this same agent, with what it did before. \
         If they want you as another of their agents, log in again with `--agent \"<its name>\"`."
    )
}

/// How often entering the workspace is tried after a login: a server that has
/// just certified this device answers a moment later.
const ENTER_TRIES: u32 = 3;

/// Enter the workspace, asking again a few times with a growing pause.
pub(crate) async fn connect_retrying(key: &SigningKey, device: &Device) -> Result<Entered> {
    let mut pause = std::time::Duration::from_secs(1);
    let mut last = None;
    for _ in 0..ENTER_TRIES {
        match connect(key, device, None).await {
            Ok(entered) => return Ok(entered),
            Err(error) => {
                debug_why("entering the workspace", &error);
                last = Some(error);
                tokio::time::sleep(pause).await;
                pause = pause.saturating_mul(2);
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!(UNREACHED)))
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

/// The signed invitation a link carries in its fragment (`#/w/<workspace>?invite=<text>`), as the
/// bytes the server signed.
fn invitation_in(link: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let text = link
        .split_once('#')
        .map(|(_, fragment)| fragment)
        .and_then(|fragment| fragment.split_once('?'))
        .and_then(|(_, query)| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix("invite="))
        })
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "This link carries no invitation: ask the person who invited you for the whole \
                 link, the part after # included."
            )
        })?;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| anyhow!("This link's invitation is damaged: ask for it again."))
}

/// `tilo login <invitation link>`: this computer joins the workspace a member invited the person
/// into, as the person kept here (made now, named `name`, when there is none). The link carries
/// the signed invitation and, in it, the workspace's devices' own signed iroh addresses: this
/// computer's daemon joins at one that holds the workspace's keys, over iroh (through its packet
/// relay when it is behind a NAT), and keeps the workspace ([`Calls::account`]'s
/// `account/workspaces#join`). Nothing is fetched from a web origin: a server on someone's laptop
/// is joined as one in a data centre is.
///
/// # Errors
/// The link carries no valid invitation, the daemon is not reached, or the workspace refuses it.
pub fn login_invited(link: &str, name: Option<&str>, agent: Option<&str>) -> Result<ExitCode> {
    trust_test_certificates()?;
    let invitation = invitation_in(link)?;
    let verified = tilo_workspace_join::decode_and_verify_invitation(
        &invitation,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| {
        debug_why("reading the invitation", &error);
        anyhow!("This invitation is not valid any more: ask the person who sent it for a new one.")
    })?;
    let grant = verified.grant().clone();
    // The name the person gave is theirs in the workspace; the device keeps a label of its own.
    let asked = name.map(str::trim).filter(|name| !name.is_empty());
    let (mut person, made) = crate::identity::ensure(asked.or(grant.invitee_name.as_deref()))?;
    if let Some(name) = asked {
        crate::identity::keep_name(&mut person, name)?;
    }
    if made {
        println!(
            "This computer now holds a Tilo identity for {}: `tilo login --new-identity` prints its key.",
            person.who()
        );
    }
    let agent = agent::given(agent);
    ensure_daemon()?;
    let runtime = runtime()?;
    let client = runtime.block_on(Calls::open_with(
        crate::api::Reach::default(),
        crate::mcp::reach_daemon,
    ))?;
    let invite = tilo_workspace_join::encode_invitation_text(&invitation);
    let joined = runtime
        .block_on(client.account("account/workspaces", "join", &json!({"invite": invite})))
        .map_err(|error| {
            debug_why("joining the workspace", &error);
            anyhow!(
                "{error:#}. The workspace's server may be asleep or offline: try again in a \
                 minute, or ask the person who invited you for a new link."
            )
        })?;
    println!(
        "Joined: this computer is one of {} devices in the workspace.\n{}",
        person.whose(),
        which_agent(&agent.name)
    );
    let became = json!({
        "workspace": joined
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or(&grant.workspace_id.to_string()),
    });
    entered_after_login(&runtime, &client, &became, &agent, false)?;
    Ok(ExitCode::SUCCESS)
}

/// `tilo login --server`: log this computer in at the server `host` names (its `web-host.json`
/// line, or that file's path) as the device the person's root, held here, certified: the computer
/// that holds the root needs no ticket. The daemon serving the login then takes in every workspace
/// of the person's that server keeps, and answers its asks for the content it holds of them.
///
/// # Errors
/// This computer holds no identity, `host` is no server's line, the server is not reached or keeps
/// no workspace of the person's, or the login cannot be kept.
pub fn login_at_server(host: &str) -> Result<ExitCode> {
    trust_test_certificates()?;
    if crate::identity::held()?.is_none() {
        bail!(
            "Only a computer that holds your Tilo identity logs in at a server by itself: log in \
             here with a ticket from `tilo device add` on one of your devices."
        );
    }
    let line = match std::fs::read_to_string(host.trim()) {
        Ok(read) => read,
        Err(_) => host.to_owned(),
    };
    tilo_api::daemon::this_computer()
        .and_then(|place| place.wait_for_native(ANSWERING))
        .map_err(anyhow::Error::msg)?;
    // The daemon logs in there, names the server among the hosts of your own workspace, and
    // enters through it from now on.
    let who = tilo_api::daemon::who::log_in_at(line.trim()).map_err(anyhow::Error::msg)?;
    let device = who.home.ok_or_else(|| anyhow!(LOGIN_ENDED))?;
    println!(
        "Logged in at {}: this computer is your own device there, and takes in every workspace of \
         yours it keeps, starting with {}.",
        host_of(&device.host_origin),
        device
            .workspace_name
            .as_deref()
            .unwrap_or(&device.workspace)
    );
    Ok(ExitCode::SUCCESS)
}

/// What the agent does once Tilo's tools are in: the one visible thing that
/// tells the person, in their own tab, that it is there.
pub(crate) fn first_act(agent: &Agent) -> String {
    format!(
        "Start with the tilo_ls tool, which lists what is in their workspace, then do \
         one visible thing so they see you working: \
         tilo_do with {{\"path\": \"notes#open\"}}, then tilo_do with \
         {{\"path\": \"notes#new-note\", \"args\": {{\"title\": \"Hello from {}\"}}}}. Their tab \
         shows the note as you write it.",
        agent.name
    )
}

/// The streamable HTTP MCP server that reaches this computer's workspace, when one does:
/// tilo.sh's for a workspace on tilo.sh's own server. A workspace on another server (one joined
/// over iroh, a company's own) is reached through the stdio server alone.
fn http_mcp() -> Option<String> {
    let device = home()?;
    let domain = host_of(&device.host_origin);
    let on_tilo_sh =
        device.server_address.is_none() && (domain == "tilo.sh" || domain.ends_with(".tilo.sh"));
    on_tilo_sh.then(|| agent::MCP_URL.to_owned())
}

/// Where an agent finds Tilo's MCP server: the stdio server `tilo mcp`, and the streamable HTTP
/// server `http` where one reaches the workspace.
fn mcp_servers(tilo: &Path, http: Option<&str>) -> String {
    let stdio = format!("the stdio server {} with the argument mcp", tilo.display());
    match http {
        Some(http) => format!("{stdio}, or the streamable HTTP server {http}"),
        None => stdio,
    }
}

/// What `tilo login` prints last: what the agent does next, in order. `http` is the streamable
/// HTTP MCP server that reaches the workspace, when one does ([`http_mcp`]).
fn next_steps(agent: &Agent, tilo: &Path, http: Option<&str>, added: bool) -> String {
    let mut steps: Vec<String> = Vec::new();
    if !added {
        steps.push(format!(
            "Add Tilo's MCP server to yourself, as the server tilo: {}. Put it where your own MCP \
             configuration keeps servers, beside the ones already there, with your own tooling.",
            mcp_servers(tilo, http)
        ));
        steps.push(format!(
            "Run `{tilo} agent check --config <its path>`: it says whether that file holds \
             Tilo's server.",
            tilo = tilo.display()
        ));
    }
    steps.push(
        "If your tools load only when you start, ask the person to restart you with this \
         conversation kept."
            .to_owned(),
    );
    steps.push(format!(
        "Call the tilo_ls tool to see what is in their workspace, and tell them what you can do \
         in Tilo: {WHAT_TILO_DOES}."
    ));
    steps.push(format!(
        "Then do one visible thing so they see you working: tilo_do with \
         {{\"path\": \"notes#open\"}}, then tilo_do with \
         {{\"path\": \"notes#new-note\", \"args\": {{\"title\": \"Hello from {}\"}}}}. Their tab \
         shows the note as you write it.",
        agent.name
    ));
    let numbered: Vec<String> = steps
        .iter()
        .enumerate()
        .map(|(index, step)| format!("{}. {step}", index.saturating_add(1)))
        .collect();
    format!("Next, in order:\n{}", numbered.join("\n"))
}

/// What a string that is no ticket says.
fn not_a_ticket(text: &str) -> anyhow::Error {
    let shown: String = text.trim().chars().take(24).collect();
    anyhow!(
        "{shown} is not a Tilo ticket; one starts with tilo1. On one of your computers, \
         `tilo device add` prints one."
    )
}

/// `tilo login <ticket> [--agent NAME] [--label NAME]`: this computer becomes one of the
/// person's devices, as the device that issued the ticket has their root certify it, and enters
/// their workspace.
///
/// # Errors
/// The text is no ticket, the issuing device is not reached, the ticket is spent or wrong, the
/// person declined, or the answer cannot be kept.
pub fn login(ticket: &str, agent: Option<&str>, named: Option<&str>) -> Result<ExitCode> {
    let parsed = Ticket::parse(ticket).map_err(|_| not_a_ticket(ticket))?;
    if parsed.direction == Direction::SignIn {
        bail!(
            "That ticket signs a browser in: run `tilo device add {}` with it on this computer.",
            ticket.trim()
        );
    }
    let agent = agent::given(agent);
    if tilo_api::calls::desktop_socket().is_some() {
        bail!(
            "This computer's Tilo desktop is one of your devices already: `tilo` commands and \
             agents here act through it, with no login of their own."
        );
    }
    let runtime = runtime()?;
    // The label names the agent first, which is how the person's devices tell which runs here.
    let said = label(
        &agent.name,
        named.map_or_else(hostname, str::to_owned).as_str(),
    );
    ensure_daemon()?;
    eprintln!("Asking your device to add this computer; it may ask you to approve it…");
    // This computer's daemon logs in, whether a person's certificate names it already or not, and
    // its person's part starts again as the device the ticket made of it.
    let who = tilo_api::daemon::who::log_in(ticket.trim(), &agent.name, &said)
        .map_err(anyhow::Error::msg)?;
    let became = who
        .home
        .as_ref()
        .map(tilo_api::daemon::enter::logged_in)
        .ok_or_else(|| anyhow!(LOGIN_ENDED))?;
    let label = became
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("this computer");
    let days = became.get("days").and_then(Value::as_u64).unwrap_or(0);
    println!("{}", logged_in_said(label, days, &agent.name));
    let client = runtime.block_on(Calls::open_with(
        crate::api::Reach::default(),
        crate::mcp::reach_daemon,
    ))?;
    let text = parsed.encode();
    report(&runtime, &client, &text, Step::LoggedIn, &agent, None);
    let added = add_and_report(&runtime, &client, &text, &agent);
    let entered = entered_after_login(&runtime, &client, &became, &agent, added)?;
    if entered {
        report(&runtime, &client, &text, Step::Entered, &agent, None);
    } else {
        report(
            &runtime,
            &client,
            &text,
            Step::Failed,
            &agent,
            Some("this computer could not enter your workspace"),
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Add Tilo's tools to the agent's tool with the tool's own command, confirm them, and tell the
/// tab that showed the ticket what became of them.
fn add_and_report(
    runtime: &tokio::runtime::Runtime,
    client: &Calls,
    ticket: &str,
    agent: &Agent,
) -> bool {
    let Some(tool) = agent.tool else {
        return false;
    };
    let Ok(tilo) = std::env::current_exe() else {
        return false;
    };
    println!("Adding Tilo's tools to {}…", tool.title());
    let added = match agent::add_tools(tool, &tilo) {
        agent::Added::Verified => {
            println!("Tilo's tools are added to {}.", tool.title());
            report(runtime, client, ticket, Step::ToolsAdded, agent, None);
            report(runtime, client, ticket, Step::Check, agent, None);
            true
        }
        agent::Added::Unverified => {
            println!(
                "Tilo's tools are added to {}; its own configuration is one Tilo does not read \
                 back.",
                tool.title()
            );
            true
        }
        agent::Added::Failed(said) => {
            println!("Tilo's tools are not in {} yet: {said}.", tool.title());
            report(
                runtime,
                client,
                ticket,
                Step::Failed,
                agent,
                Some(&format!("adding Tilo's tools to {} failed", tool.title())),
            );
            false
        }
    };
    println!("Restart {}: {}", tool.title(), tool.restart());
    report(runtime, client, ticket, Step::Restart, agent, None);
    added
}

/// Tell the tab that showed `ticket` one step of the login, as this computer's daemon reports
/// it there. A report the host did not take never fails the login: the tab shows what it heard.
pub(crate) fn report(
    runtime: &tokio::runtime::Runtime,
    client: &Calls,
    ticket: &str,
    step: Step,
    agent: &Agent,
    detail: Option<&str>,
) {
    let reported = runtime.block_on(async {
        let workspace = client.founded_workspace().await?;
        client
            .act(
                Some(&workspace),
                "devices",
                REPORTED,
                &json!({"ticket": ticket, "step": step, "agent": agent.name, "detail": detail}),
                "Report a login's step",
            )
            .await
            .map(|_| ())
    });
    if let Err(error) = reported {
        debug_why("reporting a login's step", &error);
    }
}

/// How long a command waits for this computer's daemon to serve its API, its service started.
const ANSWERING: std::time::Duration = std::time::Duration::from_secs(60);

/// Start this computer's daemon's service and wait until its native endpoint answers: a
/// connection there, where the actions a ticket is served and logged in with are called.
///
/// # Errors
/// The service did not start, or the daemon did not answer in time.
pub(crate) fn ensure_daemon() -> Result<std::os::unix::net::UnixStream> {
    tilo_api::daemon::this_computer()
        .and_then(|place| place.reach_native(ANSWERING))
        .map_err(anyhow::Error::msg)
}

/// Have this computer's daemon keep the workspace the login `became` names, and say where its
/// files and Tilo's tools are: whether it is kept here.
fn entered_after_login(
    runtime: &tokio::runtime::Runtime,
    client: &Calls,
    became: &Value,
    agent: &Agent,
    added: bool,
) -> Result<bool> {
    let tilo = std::env::current_exe().context("finding this program")?;
    let text = |name: &str| became.get(name).and_then(Value::as_str);
    let workspace = text("workspace").unwrap_or_default();
    let name = text("workspace_name");
    let place = format!(
        "{} on {}",
        name.unwrap_or("the workspace"),
        host_of(text("host_origin").unwrap_or_default())
    );
    let entered = runtime.block_on(client.account(
        "account/workspaces",
        "enter",
        &json!({ "workspace": workspace }),
    ));
    let entered = match entered {
        Ok(_) => {
            println!("Entered {place}.");
            true
        }
        Err(error) => {
            debug_why("keeping the workspace on this computer", &error);
            println!("Logged in, but this computer can't open {place} yet: {error:#}");
            false
        }
    };
    match crate::folders::after_login(workspace, name) {
        Ok(folder) => println!(
            "Its files are in {}, kept in sync both ways (`tilo folders` lists every workspace's \
             folder).",
            folder.display()
        ),
        Err(error) => {
            debug_why("putting the workspace's files in a folder", &error);
            println!(
                "Its files aren't in a folder on this computer yet: `tilo folders` asks for it again."
            );
        }
    }
    println!();
    println!("{}", next_steps(agent, &tilo, http_mcp().as_deref(), added));
    Ok(entered)
}

/// A computer logged in below `dir` for a test, as `tilo login` leaves one: `env` names `dir`'s
/// `state`, `identity` and `run` as this account's state directory, this computer's identity root
/// and its runtime directory, `key` becomes its device key with `device`'s certificate filed for
/// it, and a daemon answers `account#who` at its native endpoint with `device` as where it
/// enters.
#[cfg(test)]
pub(crate) fn keep_for_test(
    env: &mut tilo_test_env::ScopedEnv,
    dir: &Path,
    key: &SigningKey,
    device: &Device,
) {
    use std::io::{BufRead as _, Write as _};
    std::fs::create_dir_all(dir.join("run")).unwrap();
    env.set("XDG_STATE_HOME", dir.join("state"))
        .set("TILO_IDENTITY_ROOT", dir.join("identity"))
        .set("XDG_RUNTIME_DIR", dir.join("run"));
    let place = Place::here(crate::identity::paths()).unwrap();
    tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
    tilo_device_identity::login::file_certificate(&place, &device.cert).unwrap();
    let native = tilo_api::daemon::this_computer().unwrap().native();
    let listener = std::os::unix::net::UnixListener::bind(&native).unwrap();
    let who = json!({
        "person": hex::encode(device.cert.user_public_key),
        "device": hex::encode(device.cert.device_public_key),
        "label": device.label(),
        "claimed": true,
        "home": tilo_api::daemon::who::home_answer(device),
    });
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut lines = std::io::BufReader::new(&stream);
            let mut line = String::new();
            if lines.read_line(&mut line).is_ok() {
                let _ = writeln!(&stream, "{who}");
            }
        }
    });
}

/// A member certificate `root` signs for `key`, named `label`, for a test.
#[cfg(test)]
pub(crate) fn certified_for_test(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
) -> tilo_user_identity::DeviceCert {
    let now = tilo_clock::unix_now_ns();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(60_000_000_000),
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    cert
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_api::daemon::enter::{TOO_OLD, UNREACHED};
    use tilo_device_identity::login::{LOGIN_DAMAGED, file_certificate};

    /// The person's root, this computer's device key, and the login they made of it.
    fn device() -> (SigningKey, SigningKey, Device) {
        let root = SigningKey::from_bytes(&[3; 32]);
        let key = SigningKey::from_bytes(&[7; 32]);
        let cert = certified_for_test(&root, &key, "Reviewer on build-box");
        let device = Device {
            site: "https://tilo.sh/".into(),
            host_origin: "https://eu1.tilo.sh".into(),
            workspace: "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0".into(),
            workspace_name: Some("Studio".into()),
            agent: "Reviewer".into(),
            server_cert: certified_for_test(&root, &root, "eu1.tilo.sh"),
            cert,
            server_address: None,
        };
        (root, key, device)
    }

    fn listed(
        workspace: &str,
        role: &str,
        made_at_ms: i64,
    ) -> tilo_server_protocol::workspaces::PersonalWorkspace {
        tilo_server_protocol::workspaces::PersonalWorkspace {
            workspace: workspace.into(),
            name: workspace.to_uppercase(),
            host: None,
            role: Some(role.into()),
            labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            made_at_ms: Some(made_at_ms),
            unattended: false,
            enter_at: None,
        }
    }

    #[test]
    fn a_login_naming_no_workspace_starts_in_the_first_of_the_persons_own() {
        let several = [
            listed("lounge", "editor", 1),
            listed("studio", "owner", 30),
            listed("notes", "owner", 20),
        ];
        assert_eq!(choose_workspace(&several, None).unwrap().0, "notes");
        assert_eq!(
            choose_workspace(&several, Some("STUDIO")).unwrap().0,
            "studio"
        );
        let invited = [listed("lounge", "editor", 5), listed("club", "viewer", 2)];
        assert_eq!(choose_workspace(&invited, None).unwrap().0, "club");
        assert!(choose_workspace(&[], None).is_err());
        assert!(choose_workspace(&several, Some("elsewhere")).is_err());
    }

    #[test]
    fn an_invitation_link_carries_its_invitation_in_the_fragment() {
        assert_eq!(
            invitation_in(
                "https://tilo.sh/#/w/6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b?invite=c2lnbmVk"
            )
            .unwrap(),
            b"signed"
        );
        assert!(
            invitation_in("https://tilo.sh/?invite=c2lnbmVk").is_err(),
            "outside the fragment"
        );
        assert!(
            invitation_in("https://tilo.sh/#/w/x?invite=").is_err(),
            "an empty one"
        );
    }

    #[test]
    fn a_login_files_a_certificate_for_this_computers_one_device_key() {
        let dir = tempfile::tempdir().unwrap();
        let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
        let place = Place::at(dir.path().join("device"), keys.clone());
        let (root, key, device) = device();
        let this = key.verifying_key().to_bytes();
        assert!(
            !tilo_device_identity::login::claimed(&place, &this),
            "nobody logged in yet"
        );
        assert!(
            file_certificate(&place, &device.cert).is_err(),
            "a login certifies this computer's device key, and it has none yet"
        );
        tilo_device_identity::persist::adopt_device_key(&keys, &key.to_bytes()).unwrap();
        file_certificate(&place, &device.cert).unwrap();
        assert!(tilo_device_identity::login::claimed(&place, &this));
        assert_eq!(served_device_key(&keys), Some(this));
        assert_eq!(
            tilo_device_identity::login::served_cert(&keys).unwrap(),
            device.cert,
            "the certificate is kept with the device key, for the person it names"
        );
        assert_eq!(
            tilo_device_identity::login::agent_of(&device.cert.device_label),
            "Reviewer",
            "the label names the agent that runs here"
        );
        assert!(
            !place.dir().join("login.json").exists(),
            "nothing of the login is kept beside the certificate"
        );
        // A certificate for a key this computer does not have is refused.
        let other = SigningKey::from_bytes(&[8; 32]);
        assert!(
            file_certificate(
                &place,
                &certified_for_test(&root, &other, "Reviewer on build-box")
            )
            .is_err()
        );
    }

    #[test]
    fn a_failed_entry_says_what_to_do_without_the_machinery() {
        assert_eq!(
            enter_said(&anyhow::anyhow!("device cert has expired")),
            LOGIN_ENDED
        );
        assert_eq!(
            enter_said(&anyhow::anyhow!(
                "the host speaks peer protocol 99, this device 3"
            )),
            TOO_OLD
        );
        assert_eq!(
            enter_said(&anyhow::anyhow!(
                "the workspace's host takes no device connections"
            )),
            UNREACHED
        );
        assert_eq!(enter_said(&anyhow::anyhow!(LOGIN_DAMAGED)), LOGIN_DAMAGED);
        // A workspace that answered and refused is never called unreachable: its reason is said.
        assert_eq!(
            enter_said(&anyhow::anyhow!(
                "The workspace did not let this device in: the name is empty"
            )),
            "The workspace did not let this device in: the name is empty."
        );
    }

    #[test]
    fn a_login_takes_a_ticket_and_names_what_else_it_was_given() {
        let said = login("K7Q-MX2P", None, None).unwrap_err().to_string();
        assert!(said.contains("is not a Tilo ticket"), "{said}");
        assert!(said.contains("tilo device add"), "{said}");
        let reached = tilo_device_identity::login::Reached {
            endpoint: *SigningKey::from_bytes(&[5; 32]).verifying_key().as_bytes(),
            relay: Some("https://relay.example./".into()),
            direct: vec!["192.0.2.1:4433".parse().unwrap()],
        };
        assert_eq!(
            tilo_api::daemon::enter::reached_of(
                &tilo_api::daemon::enter::address_of(&reached).unwrap()
            ),
            reached
        );
    }

    tilo_test_env::scenario! {
        fn the_agent_is_told_its_next_steps;

        phase the_agent_is_told_to_add_the_server_itself;
        // Every agent adds Tilo's server to its own configuration, checks it, and says what
        // it does after.
        {
            let tilo = Path::new("/home/me/.local/bin/tilo");
            let other = agent::named("Reviewer");
            let next = next_steps(&other, tilo, Some(agent::MCP_URL), false);
            let steps: Vec<&str> = next.lines().collect();
            assert_eq!(steps[0], "Next, in order:");
            assert!(
                steps[1].starts_with(
                    "1. Add Tilo's MCP server to yourself, as the server tilo: the stdio server \
                 /home/me/.local/bin/tilo with the argument mcp, or the streamable HTTP server \
                 https://mcp.tilo.sh"
                ),
                "{next}"
            );
            assert!(
                steps[2].starts_with("2. Run `/home/me/.local/bin/tilo agent check --config"),
                "{next}"
            );
            assert!(steps[3].contains("restart you"), "{next}");
            assert!(steps[4].starts_with("4. Call the tilo_ls tool"), "{next}");
            assert!(
                steps[5].contains("\"path\": \"notes#open\"")
                    && steps[5].contains("\"title\": \"Hello from Reviewer\""),
                "the agent does one thing the person sees: {next}"
            );
            // A workspace no HTTP server reaches (one on a company's own server, joined over
            // iroh) is offered the stdio server alone.
            let elsewhere = next_steps(&other, tilo, None, false);
            assert!(
                elsewhere.contains(
                    "the stdio server /home/me/.local/bin/tilo with the argument mcp. Put it"
                ),
                "{elsewhere}"
            );
            assert!(!elsewhere.contains("streamable HTTP"), "{elsewhere}");
        }

        phase the_check_tells_the_agent_what_it_does_first;
        // What the agent's own check says once the server is in: the one visible
        // thing it does for the person.
        {
            let said = first_act(&agent::named("Scout"));
            assert!(said.starts_with("Start with the tilo_ls tool"), "{said}");
            assert!(said.contains("notes#new-note"), "{said}");
            assert!(said.contains("Hello from Scout"), "{said}");
        }
    }
}
