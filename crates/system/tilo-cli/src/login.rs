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
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_api::daemon::DaemonPlace;
use tilo_api::server::ServerAddress;
use tilo_device_identity::login::{Direction, REPORTED, Step, Ticket};
use tilo_server_client::CertifyError;
use tilo_server_client::LinkError;
use tilo_session_client::Outbox as _;
use tilo_session_client::device::Device as Entered;

use crate::agent::{self, Agent};
use crate::api::Client;
pub(crate) use tilo_api::daemon::enter::{
    IROH_ONLY, LOGIN_ENDED, NOT_LOGGED_IN, TOO_OLD, UNREACHED, choose_workspace, connect,
    debug_why, enter_said, host_of, server_of, signed_over_iroh,
};
pub use tilo_device_identity::login::{Device, LOGIN_DAMAGED, Place, load, load_public, store};

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

pub fn logged_in() -> bool {
    Place::here(crate::identity::paths()).is_ok_and(|place| place.logged_in())
}

/// This computer's one device key, the one its session keeps, made now when it has none: every
/// login of this computer is a certificate for it, never a key of its own.
///
/// # Errors
/// A Tilo desktop serves this login, so it is a device already, or the key cannot be read or
/// kept.
pub(crate) fn this_device_key() -> Result<SigningKey> {
    if tilo_api::client::desktop_socket().is_some() {
        bail!(
            "This computer's Tilo desktop is one of your devices already: `tilo` commands and \
             agents here act through it, with no login of their own."
        );
    }
    let (key, _) =
        tilo_device_identity::persist::load_or_create_device_key(&crate::identity::paths())
            .context("reading this computer's device key")?;
    Ok(key)
}

/// `tilo device public-key`: this computer's device key as hex, for [`this_device_public_key`].
///
/// # Errors
/// The key cannot be read or kept.
pub(crate) fn print_public_key() -> Result<ExitCode> {
    println!(
        "{}",
        hex::encode(this_device_key()?.verifying_key().to_bytes())
    );
    Ok(ExitCode::SUCCESS)
}

/// This computer's device key, made now when it has none, read by a child of this program: a
/// process that reads the private key is undumpable, and this computer's daemon refuses API
/// calls from a process whose namespaces it cannot read. A command that calls the daemon after
/// it knows which device this is keeps the private key out of its process until those calls end.
///
/// # Errors
/// The child cannot run, or says why it could not read or keep the key.
pub(crate) fn this_device_public_key() -> Result<[u8; 32]> {
    let tilo = std::env::current_exe().context("finding this program")?;
    let out = std::process::Command::new(tilo)
        .args(["device", "public-key"])
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("reading this computer's device key")?;
    if !out.status.success() {
        bail!("reading this computer's device key failed");
    }
    hex::decode(String::from_utf8_lossy(&out.stdout).trim())
        .ok()
        .and_then(|key| key.try_into().ok())
        .ok_or_else(|| anyhow!("reading this computer's device key gave no key"))
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
    let mut fitted = format!("{agent} on {host}");
    let room = tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
    fitted.truncate(
        (0..=room.min(fitted.len()))
            .rev()
            .find(|end| fitted.is_char_boundary(*end))
            .unwrap_or(0),
    );
    fitted
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

/// What an invitation the workspace's server did not take says to the person.
fn invitation_said(error: &CertifyError) -> String {
    match error {
        CertifyError::Denied(reason) => {
            format!("{reason}. Ask the person who invited you for a new link.")
        }
        CertifyError::Codec(_) => TOO_OLD.to_owned(),
        CertifyError::Transport(link) => link_said(link),
        CertifyError::Auth(error) => auth_said(error),
        CertifyError::Expired
        | CertifyError::Request(_)
        | CertifyError::Sign(_)
        | CertifyError::Iroh(_)
        | CertifyError::Io(_) => UNREACHED.to_owned(),
    }
}

fn link_said(error: &LinkError) -> String {
    match error {
        LinkError::Unavailable(_) | LinkError::Refused(_) => UNREACHED.to_owned(),
        LinkError::ServerChanged => {
            "This workspace has a new home. Ask the person for a new code from their Tilo tab \
             (Set up with your agent)."
                .to_owned()
        }
        LinkError::Malformed(_) | LinkError::PeerVersion { .. } => TOO_OLD.to_owned(),
        LinkError::Sign(_) => LOGIN_DAMAGED.to_owned(),
        LinkError::DeviceRevoked => LOGIN_ENDED.to_owned(),
        LinkError::CertificateRejected => {
            "The workspace refused this computer's certificate. `tilo login` again makes a new one."
                .to_owned()
        }
    }
}

/// What a handshake this computer did not pass says.
fn auth_said(error: &tilo_server_protocol::auth::AuthError) -> String {
    use tilo_server_protocol::auth::AuthError;
    use tilo_user_identity::DeviceCertError;
    match error {
        AuthError::Cert(DeviceCertError::Expired) | AuthError::Revoked => LOGIN_ENDED.to_owned(),
        AuthError::Cert(_) | AuthError::PolicyReject => {
            "Tilo didn't let this computer into the workspace. Ask the person to allow it in \
             their Tilo tab, then try again."
                .to_owned()
        }
        AuthError::BadChallengeSig | AuthError::EndpointMismatch => TOO_OLD.to_owned(),
    }
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

/// The server an invitation names, where it is dialed over iroh: one that holds the workspace's
/// keys, since only such a server lets a new member in.
fn invited_server(
    grant: &tilo_workspace_join::InviteGrant,
) -> Result<tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1> {
    let servers: Vec<_> = grant
        .transport
        .iter()
        .filter_map(|hint| {
            tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(hint)
                .ok()
        })
        .filter(|server| server.workspace_id == grant.workspace_id)
        .collect();
    servers
        .iter()
        .find(|server| server.holds_keys)
        .or_else(|| servers.first())
        .cloned()
        .ok_or_else(|| anyhow!("This invitation names no server to dial: ask for a new one."))
}

/// Redeem `invitation` on a session with the server at `address`, as `device` shown to the
/// workspace's people as `shown_as`: the workspace's name, which the server answers.
async fn redeem_over_iroh(
    key: &SigningKey,
    device: &Device,
    address: &ServerAddress,
    invitation: &[u8],
    shown_as: &str,
) -> Result<Option<String>> {
    let signed = signed_over_iroh(device, address)?;
    let session = Entered::connect(&signed, Arc::new(key.clone()));
    let admitted = async {
        session.connected().await.map_err(|refusal| {
            debug_why("dialing the workspace's server", &refusal);
            anyhow!(
                "The workspace's server can't be reached from here right now: the computer \
                 it is on may be asleep or offline. Try again in a minute."
            )
        })?;
        let admission = session
            .redeem(invitation, shown_as)
            .await
            .map_err(|error| {
                debug_why("redeeming the invitation", &error);
                anyhow!("{}", invitation_said(&error))
            })?;
        // A person without a name stays unnamed until they give one.
        if !shown_as.is_empty() {
            name_person(&session, &admission.workspace, shown_as).await;
        }
        anyhow::Ok(admission)
    }
    .await;
    session.close();
    Ok(Some(admitted?.name))
}

/// Announce `entry`, a revocation the person's root signed, to every workspace `session` is in.
///
/// # Errors
/// The session ended first.
pub(crate) fn announce_revocation(
    session: &Entered,
    entry: tilo_user_identity::RevocationEntry,
) -> Result<()> {
    session
        .session()
        .send(tilo_session_client::Cmd::Revoke(entry))
        .map_err(|_| anyhow!("the session ended before the revocation was sent"))
}

/// How long naming the person in a workspace they just joined may take.
const NAMING_WITHIN: std::time::Duration = std::time::Duration::from_secs(15);

/// Tell `workspace` that the person `session` is a device of goes by `name`, as a page tells it
/// as it joins: what its member list and everyone's lists of people show for them, rather than
/// this device's label. A workspace that does not answer keeps the name it admitted them with.
async fn name_person(session: &Entered, workspace: &str, name: &str) {
    let asked = tilo_server_protocol::workspaces::WorkspacesRequest::Name {
        workspace: workspace.to_owned(),
        name: name.to_owned(),
    };
    let answer = match session.session().ask_workspaces(asked) {
        Ok(receipt) => tokio::time::timeout(NAMING_WITHIN, receipt).await,
        Err(closed) => {
            debug_why("naming the person in the workspace", &closed);
            return;
        }
    };
    match answer {
        Ok(Ok(Ok(_))) => {}
        Ok(Ok(Err(refused))) => debug_why("naming the person in the workspace", &refused),
        Ok(Err(_)) => debug_why("naming the person in the workspace", &"the session ended"),
        Err(_) => debug_why("naming the person in the workspace", &"no answer in time"),
    }
}

/// `tilo login <invitation link>`: this computer joins the workspace a member invited the person
/// into, as the person kept here (made now, named `name`, when there is none). The link carries
/// the signed invitation and, in it, the server's own signed iroh address, so the server is dialed
/// over iroh (through its packet relay when it is behind a NAT) and nothing is fetched from a web
/// origin: a server on someone's laptop is joined as one in a data centre is.
///
/// # Errors
/// The link carries no valid invitation, the server is not reached, or the workspace refuses it.
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
    let server = invited_server(&grant)?;
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
    let key = this_device_key()?;
    let label = label(&agent.name, &hostname());
    let cert = held_device_cert(&person, &key, &label)?;
    let address = ServerAddress {
        endpoint_addr: server.endpoint_addr.clone(),
        relay_url: server
            .packet_relay_url
            .parse::<iroh::RelayUrl>()
            .ok()
            .map(|url| url.to_string()),
        web: None,
    };
    let shown_as = asked
        .or_else(|| Some(person.name.as_str()).filter(|name| !name.is_empty()))
        .or(grant.invitee_name.as_deref())
        .unwrap_or_default()
        .to_owned();
    let device = Device {
        site: format!("{DEFAULT_SITE}/"),
        host_origin: format!("{IROH_ONLY}{}", server.endpoint_addr.id),
        workspace: grant.workspace_id.to_string(),
        workspace_name: None,
        agent: agent.name.clone(),
        cert,
        server_cert: server.device_certificate,
        server_address: Some(address.clone()),
    };
    let runtime = runtime()?;
    let workspace_name = runtime.block_on(redeem_over_iroh(
        &key,
        &device,
        &address,
        &invitation,
        &shown_as,
    ))?;
    let device = Device {
        workspace_name,
        ..device
    };
    store(&Place::here(crate::identity::paths())?, &device)?;
    println!(
        "Joined: this computer is one of {} devices, \"{}\", in the workspace.\n{}",
        person.whose(),
        device.label(),
        which_agent(&agent.name)
    );
    entered_after_login(&runtime, &key, &device, &agent, false)?;
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
    let person = crate::identity::held()?.ok_or_else(|| {
        anyhow!(
            "Only a computer that holds your Tilo identity logs in at a server by itself: log in \
             here with a ticket from `tilo device add` on one of your devices."
        )
    })?;
    let text = match std::fs::read_to_string(host.trim()) {
        Ok(read) => read,
        Err(_) => host.to_owned(),
    };
    let host: tilo_server_protocol::web::PageHost = serde_json::from_str(text.trim())
        .map_err(|error| anyhow!("That is not a server's line from its web-host.json: {error}"))?;
    let runtime = runtime()?;
    let device = runtime.block_on(tilo_api::daemon::enter::log_in_at(
        &person.device,
        &person.cert,
        host,
        None,
    ))?;
    let login_place = Place::here(crate::identity::paths())?;
    store(&login_place, &device)?;
    println!(
        "Logged in at {}: this computer is your own device there, and takes in every workspace of \
         yours it keeps, starting with {}.",
        host_of(&device.host_origin),
        device
            .workspace_name
            .as_deref()
            .unwrap_or(&device.workspace)
    );
    let place = DaemonPlace::of(login_place.dir(), &person.device.verifying_key().to_bytes())
        .map_err(anyhow::Error::msg)?;
    restart_after_login(&place)?;
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

/// The streamable HTTP MCP server that reaches `device`'s workspace, when one does: the API
/// server `TILO_API_URL` names, else tilo.sh's for a workspace on tilo.sh's own server. A
/// workspace on another server (one joined over iroh, a company's own) is reached through the
/// stdio server alone.
fn http_mcp(device: &Device) -> Option<String> {
    if let Some(api) = std::env::var(tilo_api::client::API_URL_ENV)
        .ok()
        .filter(|api| !api.trim().is_empty())
    {
        return Some(api);
    }
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
    crate::identity::convert_earlier()?;
    let parsed = Ticket::parse(ticket).map_err(|_| not_a_ticket(ticket))?;
    if parsed.direction == Direction::SignIn {
        bail!(
            "That ticket signs a browser in: run `tilo device add {}` with it on this computer.",
            ticket.trim()
        );
    }
    let agent = agent::given(agent);
    let login_place = Place::here(crate::identity::paths())?;
    let device_key = this_device_public_key()?;
    let place = DaemonPlace::of(login_place.dir(), &device_key).map_err(anyhow::Error::msg)?;
    let runtime = runtime()?;
    let said = named.map_or_else(|| label(&agent.name, &hostname()), str::to_owned);
    eprintln!("Asking your device to add this computer; it may ask you to approve it…");
    // A computer no person's certificate names yet logs in itself: its daemon serves the device
    // key alone until then, with no API to ask, and takes the person's workspaces up once the
    // certificate is here.
    let claimed = tilo_device_identity::login::claimed(&login_place, &device_key);
    if claimed {
        ensure_daemon(&place)?;
        asked_daemon(&runtime, ticket, &agent, &said)?;
    } else {
        let key = this_device_key()?;
        runtime.block_on(tilo_api::daemon::enter::log_in_here(
            &key,
            &login_place,
            ticket,
            &agent.name,
            Some(&said),
        ))?;
        // The daemon serving the device alone takes the login up; one is started where none runs.
        ensure_daemon(&place)?;
    }
    // The login is kept; the daemon serving it hands this computer its workspace.
    let (key, device) = load(&login_place)?.ok_or_else(|| anyhow!(LOGIN_DAMAGED))?;
    let days = device.cert.expires_at_ns.map_or(0, |end| {
        end.saturating_sub(device.cert.issued_at_ns) / (24 * 3600 * 1_000_000_000)
    });
    println!("{}", logged_in_said(device.label(), days, &agent.name));
    let client = Client::open(crate::api::Reach {
        api: None,
        key_file: None,
        json: false,
    })?;
    let text = parsed.encode();
    report(&runtime, &client, &text, Step::LoggedIn, &agent, None);
    let added = add_and_report(&runtime, &client, &text, &agent);
    let entered = entered_after_login(&runtime, &key, &device, &agent, added)?;
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
    if claimed {
        restart_after_login(&place)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Add Tilo's tools to the agent's tool with the tool's own command, confirm them, and tell the
/// tab that showed the ticket what became of them.
fn add_and_report(
    runtime: &tokio::runtime::Runtime,
    client: &Client,
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
    client: &Client,
    ticket: &str,
    step: Step,
    agent: &Agent,
    detail: Option<&str>,
) {
    let reported = runtime.block_on(async {
        let workspace = client.founded_workspace().await?;
        client
            .act(
                &workspace,
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

/// Ask this computer's daemon, which serves a person already, to log in with `ticket`.
fn asked_daemon(
    runtime: &tokio::runtime::Runtime,
    ticket: &str,
    agent: &Agent,
    said: &str,
) -> Result<()> {
    runtime.block_on(async {
        let client = Client::open(crate::api::Reach {
            api: None,
            key_file: None,
            json: false,
        })?;
        let workspace = client.founded_workspace().await?;
        client
            .act(
                &workspace,
                "devices",
                tilo_device_identity::login::LOG_IN,
                &json!({"ticket": ticket.trim(), "agent": agent.name, "label": said}),
                "Log in",
            )
            .await?;
        Ok::<(), anyhow::Error>(())
    })
}

/// How long `tilo login` waits for the daemon it logged in to end before starting the one the
/// new login needs.
const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Start this computer's daemon and wait until its API socket is served: the actions a ticket is
/// served and logged in with are calls there.
///
/// # Errors
/// The daemon did not start, or did not serve its API in time.
pub(crate) fn ensure_daemon(place: &DaemonPlace) -> Result<()> {
    place
        .wait_for_api(RESTART_WAIT, || {
            crate::mcp::held::ensure(place).map_err(std::io::Error::other)
        })
        .map_err(anyhow::Error::msg)
}

/// Start the daemon the new login needs, once the one serving the login before it ended.
fn restart_after_login(place: &DaemonPlace) -> Result<()> {
    let started = std::time::Instant::now();
    while place.alive() || place.listening() {
        if started.elapsed() > RESTART_WAIT {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    crate::mcp::held::ensure(place).map_err(|why| anyhow!(why))
}

fn entered_after_login(
    runtime: &tokio::runtime::Runtime,
    key: &SigningKey,
    device: &Device,
    agent: &Agent,
    added: bool,
) -> Result<bool> {
    let tilo = std::env::current_exe().context("finding this program")?;
    let place = format!(
        "{} on {}",
        device.workspace_name.as_deref().unwrap_or("the workspace"),
        host_of(&device.host_origin)
    );
    let entered = runtime.block_on(async {
        match connect_retrying(key, device).await {
            Ok(entered) => {
                println!("Entered {place}.");
                entered.close();
                true
            }
            Err(error) => {
                println!(
                    "Logged in, but this computer can't open {place} yet: {}",
                    enter_said(&error)
                );
                false
            }
        }
    });
    match crate::folders::after_login(&device.workspace, device.workspace_name.as_deref()) {
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
    println!(
        "{}",
        next_steps(agent, &tilo, http_mcp(device).as_deref(), added)
    );
    Ok(entered)
}

/// A member device certificate for `key`, signed by the person's root.
fn held_device_cert(
    person: &crate::identity::Person,
    key: &SigningKey,
    label: &str,
) -> Result<tilo_user_identity::DeviceCert> {
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.public_key(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person.root)?;
    Ok(cert)
}

/// A login kept below `dir` for a test, as `tilo login` keeps one: `env` names `dir`'s `state`
/// and `identity` as this account's state directory and this computer's identity root, `key`
/// becomes its device key, and `device` its login.
#[cfg(test)]
pub(crate) fn keep_for_test(
    env: &mut tilo_test_env::ScopedEnv,
    dir: &Path,
    key: &SigningKey,
    device: &Device,
) {
    env.set("XDG_STATE_HOME", dir.join("state"))
        .set("TILO_IDENTITY_ROOT", dir.join("identity"));
    let place = Place::here(crate::identity::paths()).unwrap();
    tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
    store(&place, device).unwrap();
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
    ) -> tilo_server_session::workspaces::PersonalWorkspace {
        tilo_server_session::workspaces::PersonalWorkspace {
            workspace: workspace.into(),
            name: workspace.to_uppercase(),
            host: None,
            role: Some(role.into()),
            labels: tilo_server_session::workspaces::WorkspaceLabels::new(),
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

    tilo_test_env::scenario! {
        fn a_login_keeps_its_device_and_the_tab_it_came_from;

        phase the_login_is_a_certificate_for_this_computers_one_device_key;
        {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir().unwrap();
            let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
            let place = Place::at(dir.path().join("device"), keys.clone());
            let (root, key, device) = device();
            assert!(load(&place).unwrap().is_none(), "nobody logged in yet");
            assert!(
                store(&place, &device).is_err(),
                "a login certifies this computer's device key, and it has none yet"
            );
            tilo_device_identity::persist::adopt_device_key(&keys, &key.to_bytes()).unwrap();
            store(&place, &device).unwrap();
            assert!(place.logged_in());
            assert_eq!(
                std::fs::metadata(place.dir()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let (read, kept) = load(&place).unwrap().unwrap();
            assert_eq!(read.to_bytes(), key.to_bytes());
            assert_eq!(kept, device);
            assert_eq!(
                tilo_device_identity::persist::read_device_cert(
                    &keys.device_cert_for_user(&root.verifying_key().to_bytes())
                )
                .unwrap(),
                device.cert,
                "the certificate is kept with the device key, for the person it names"
            );
            assert!(
                !place.dir().join("key").exists(),
                "no key is kept beside the login"
            );
            // A certificate for a key this computer no longer has asks for a new login.
            let other = SigningKey::from_bytes(&[8; 32]);
            let stale = Device {
                cert: certified_for_test(&root, &other, "Reviewer on build-box"),
                ..device
            };
            tilo_device_identity::persist::write_device_cert(
                &keys.device_cert_for_user(&root.verifying_key().to_bytes()),
                &stale.cert,
            )
            .unwrap();
            let said = load(&place).unwrap_err().to_string();
            assert!(said.contains("log in again with a ticket"), "{said}");
            // A login that is not one says what to do, not what it holds.
            std::fs::write(place.dir().join("login.json"), b"{}").unwrap();
            assert_eq!(load(&place).unwrap_err().to_string(), LOGIN_DAMAGED);
        }
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
