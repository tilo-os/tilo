//! `tilo login <code>` and `tilo mcp`: this computer as one of the person's devices, for the agent
//! that runs here.
//!
//! The code is the short one the person's browser tab shows (`K7Q-MX2P`), in the prompt it copies
//! for their agent. `tilo login` resolves it at the site's home server
//! (`tilo_server_protocol::login_code`) and tells that tab each step as it does it: it makes a
//! device key here, asks the workspace's server to have the person's root (which only the tab holds)
//! certify it, keeps the key and certificate in this account's state directory, adds Tilo's tools
//! to the agent and enters the workspace the link names as the person. A tab with no identity yet
//! asks a computer that is already one of the person's devices to vouch for it.
//!
//! `tilo mcp` ([`crate::mcp`]) is the agent's MCP server over stdio, under the name `tilo`, acting
//! as that device.

use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_server_client::CertifyError;
use tilo_server_client::HttpClient;
use tilo_server_client::LinkError;
use tilo_server_client::https::{self, Origin};
use tilo_server_client::login::{PairingLink, SignIn, entry_at, sign_in};
use tilo_server_client::login_code;
use tilo_server_protocol::login_code::{self as protocol, Claimed, Report, Resolved, Step};
use tilo_server_protocol::web::API_PREFIX;
use tilo_session_client::device::Device as Entered;

use crate::agent::{self, Agent, Tool};
pub use tilo_api::client::device::{Device, LOGIN_DAMAGED, device_dir, load, store};

/// The site `tilo login` resolves codes at, unless `--site` or
/// [`SITE_ENV`] names another.
pub const DEFAULT_SITE: &str = "https://tilo.sh";
/// Names the site to resolve codes at, like `--site`.
pub const SITE_ENV: &str = "TILO_SITE";
/// For tests only: PEM files (separated as `PATH` is) whose certificates
/// HTTPS also trusts, each as a CA and as that exact server certificate.
/// Verification stays on; production trust is the system's alone.
pub const TEST_CA_ENV: &str = "TILO_TEST_CA_FILE";
/// What Tilo's tools let the agent do, as it tells the person.
const WHAT_TILO_DOES: &str = "you are in their Tilo workspace as them, where they see you by \
     name with what you do: tilo_ls lists its apps and documents, tilo_read reads one, \
     tilo_do runs their actions, and tilo_do with path terminal#run runs a shell command \
     there, all of which History keeps as your run";

/// What a login no server accepts any more says.
pub(crate) const LOGIN_ENDED: &str = "This computer's Tilo login has ended: ask the person for a new code from their Tilo tab \
     (Set up with your agent).";

/// What a tilo that speaks another version than the server says.
pub(crate) const TOO_OLD: &str = "This tilo is too old for your workspace: install it again with \
     `curl -fsSL https://tilo.sh | sh`.";

/// What a server that answers nothing usable says.
pub(crate) const UNREACHED: &str =
    "Tilo can't be reached from here right now; try again in a minute.";

/// Why something failed, for whoever runs the logs: `RUST_LOG` set says
/// this computer keeps them, and the person gets the plain sentence instead.
pub(crate) fn debug_why(what: &str, why: &dyn std::fmt::Display) {
    if std::env::var_os("RUST_LOG").is_some() {
        eprintln!("tilo: {what}: {why}");
    }
}

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

/// What a login leaves behind for a later `tilo agent check`: the tab that
/// followed it, so the agent's own confirmation reaches that tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub report: String,
    pub host_origin: String,
    #[serde(default)]
    pub tools_added: bool,
    /// Whether this computer entered the workspace, and the tab was not told
    /// yet: then the agent's confirmation is what tells it, and the screen
    /// leaves only once the tools are in.
    #[serde(default)]
    pub entered: bool,
}

/// Keep `pending` beside this computer's device key.
fn store_pending(dir: &Path, pending: &Pending) -> Result<()> {
    let _ = std::fs::remove_file(dir.join("login.json"));
    std::fs::write(dir.join("login.json"), serde_json::to_vec_pretty(pending)?)?;
    Ok(())
}

/// The login this computer last made, when it left a tab following it.
fn load_pending(dir: &Path) -> Option<Pending> {
    serde_json::from_slice(&std::fs::read(dir.join("login.json")).ok()?).ok()
}

/// Tell the tab that followed this computer's login that its agent has
/// Tilo's tools, and that this computer is in the workspace: the steps that
/// tab shows only because the server was found in the agent's own
/// configuration and answered.
///
/// `tilo agent check` calls this after confirming; `tilo login` calls it
/// itself when its own check passed. A tab that has moved on is not told
/// anything twice, and nothing is said about it.
pub(crate) fn report_tools_added() {
    let Ok(dir) = device_dir() else { return };
    let Some(mut pending) = load_pending(&dir) else {
        return;
    };
    if pending.tools_added {
        return;
    }
    let Ok(runtime) = runtime() else { return };
    let entered = pending.entered;
    runtime.block_on(async {
        let host = Origin::new(&http(), &pending.host_origin);
        let reporter = Reporter {
            host: &host,
            report: pending.report.clone(),
            agent: agent::detect_here().name,
        };
        reporter.quiet_step(Step::ToolsAdded).await;
        // What the screen leaves for: said here for an agent that added the
        // server itself, so the tools are confirmed before it goes.
        if entered {
            reporter.quiet_step(Step::Entered).await;
        }
    });
    pending.tools_added = true;
    pending.entered = false;
    let _ = store_pending(&dir, &pending);
}

pub fn logged_in() -> bool {
    device_dir().is_ok_and(|dir| dir.join("key").exists())
}

pub(crate) fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "this computer".to_owned())
}

/// `"Claude Code on build-box"`, the name this computer asks the server for,
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

/// Have the person's tab certify this computer as one of their devices, under
/// the name [`label`] names for the agent here: the server answers the name it
/// gave it.
async fn certify(
    host: &Origin,
    link: &PairingLink,
    agent: &str,
    key: &SigningKey,
) -> Result<SignIn> {
    sign_in(host, link, agent, &label(agent, &hostname()), key, |_| {})
        .await
        .map_err(|error| {
            debug_why("the person's tab certifying this computer", &error);
            anyhow!(certify_said(&error))
        })
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

/// What a login that failed says: what happened, in the person's words, and
/// the one action that fixes it.
fn certify_said(error: &CertifyError) -> String {
    match error {
        CertifyError::Expired => {
            "The person's Tilo tab didn't approve this computer in time: it approves only while \
             the tab is open. Keep the tab open, then run the same command again."
                .to_owned()
        }
        CertifyError::Denied(reason) if reason.contains("declined") => {
            "The person's Tilo tab declined this computer. Run the same command again and allow \
             it in the tab."
                .to_owned()
        }
        CertifyError::Denied(_) => {
            "Tilo didn't let this computer in. Run the same command again with a new code from \
             the person's Tilo tab."
                .to_owned()
        }
        CertifyError::Request(_) | CertifyError::Sign(_) => {
            "Tilo couldn't ask the person's Tilo tab to approve this computer. Try again in a \
             minute."
                .to_owned()
        }
        CertifyError::Transport(link) => link_said(link),
        CertifyError::Iroh(_) | CertifyError::Io(_) => UNREACHED.to_owned(),
        CertifyError::Codec(_) => TOO_OLD.to_owned(),
        CertifyError::Auth(error) => auth_said(error),
    }
}

/// What a server that answered nothing usable says.
fn link_said(error: &LinkError) -> String {
    match error {
        LinkError::Unavailable(_) | LinkError::Refused(_) => UNREACHED.to_owned(),
        LinkError::HostChanged => {
            "This workspace has a new home. Ask the person for a new code from their Tilo tab \
             (Set up with your agent)."
                .to_owned()
        }
        LinkError::Malformed(_) | LinkError::PeerVersion { .. } => TOO_OLD.to_owned(),
        LinkError::Sign(_) => LOGIN_DAMAGED.to_owned(),
        LinkError::DeviceRevoked => LOGIN_ENDED.to_owned(),
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

/// What a failure to enter the workspace says, from the error's own chain.
pub(crate) fn enter_said(error: &anyhow::Error) -> String {
    let chain = format!("{error:#}");
    let says = |needle: &str| chain.contains(needle);
    if says("Tilo login has ended") || says("device cert has expired") || says("revoked") {
        LOGIN_ENDED.to_owned()
    } else if says("Tilo login is damaged")
        || says("not hex")
        || says("names no person")
        || says("postcard")
    {
        LOGIN_DAMAGED.to_owned()
    } else if says("peer protocol")
        || says("handshake frame does not decode")
        || says("malformed answer")
        || says("answers in a way this device does not read")
    {
        TOO_OLD.to_owned()
    } else if says("did not let this key in") || says("did not let this computer in") {
        "Tilo didn't let this computer into the workspace. Ask the person to allow it in their \
         Tilo tab, then try again."
            .to_owned()
    } else {
        UNREACHED.to_owned()
    }
}

/// How often entering the workspace is tried after a login: a server that has
/// just certified this device answers a moment later.
const ENTER_TRIES: u32 = 3;

/// Enter the workspace, asking again a few times with a growing pause.
async fn connect_retrying(key: &SigningKey, device: &Device) -> Result<Entered> {
    let mut pause = std::time::Duration::from_secs(1);
    let mut last = None;
    for _ in 0..ENTER_TRIES {
        match connect(key, device).await {
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

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

/// Enter the workspace `device` names, as that device.
pub(crate) async fn connect(key: &SigningKey, device: &Device) -> Result<Entered> {
    let link = PairingLink::parse(&format!(
        "{}#/@{}/w/{}?pair={}",
        device.site,
        host_of(&device.host_origin),
        device.workspace,
        device.user
    ))
    .map_err(|error| anyhow!(error))?;
    let host_web = Origin::new(&http(), &device.host_origin);
    let info = https::host_info(&host_web).await.map_err(|error| {
        debug_why("asking the workspace's host", &error);
        anyhow!(UNREACHED)
    })?;
    let host = info
        .address
        .as_ref()
        .map(tilo_server_client::Host::published)
        .ok_or_else(|| anyhow!(UNREACHED))?;
    let (cert, host_cert) = device.certs()?;
    let signed = SignIn {
        link,
        agent: device.agent.clone(),
        label: device.label.clone(),
        host,
        cert,
        host_cert,
    };
    let entry = entry_at(&Origin::new(&http(), &device.site), &device.host_origin).await;
    Ok(Entered::enter(entry.as_ref(), &signed, Arc::new(key.clone())).await?)
}

/// The site `--site` or [`SITE_ENV`] names, else [`DEFAULT_SITE`], as an
/// HTTPS origin.
fn site_origin(flag: Option<&str>) -> Result<String> {
    let named = flag
        .map(str::to_owned)
        .or_else(|| std::env::var(SITE_ENV).ok())
        .filter(|site| !site.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SITE.to_owned());
    let url = tilo_net::Url::parse(named.trim())
        .with_context(|| format!("{named} is not a site's address"))?;
    if !tilo_net::secure_origin(&url) {
        bail!("{named} is not an https:// site (or an http:// one on this computer)");
    }
    Ok(url.origin().ascii_serialization())
}

/// The site's home server, as its `entry.json` names it (`home.server.origin`).
///
/// A site that answers nothing, or nothing this tilo reads, is said as it
/// is: Tilo cannot be reached from here right now.
async fn home_host(site: &Origin) -> Result<String> {
    let (status, body) = site.get("/entry.json").await.map_err(|error| {
        debug_why(&format!("reading {}'s entry.json", site.name()), &error);
        anyhow!(UNREACHED)
    })?;
    let entry: Value = serde_json::from_slice(&body).map_err(|error| {
        debug_why(&format!("reading {}'s entry.json", site.name()), &error);
        anyhow!(UNREACHED)
    })?;
    if status != 200 {
        debug_why(&format!("reading {}'s entry.json", site.name()), &status);
        bail!(UNREACHED);
    }
    let origin = entry
        .pointer("/home/host/origin")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!(UNREACHED))?;
    let url = tilo_net::Url::parse(origin).map_err(|error| {
        debug_why("reading the site's host address", &error);
        anyhow!(UNREACHED)
    })?;
    if !tilo_net::secure_origin(&url) {
        debug_why("the site's host address", &origin);
        bail!(UNREACHED);
    }
    Ok(url.origin().ascii_serialization())
}

/// What the code names, spending it.
///
/// A code the server does not know is asked about again for a moment: the server
/// keeps a code from the moment the tab showing it follows it, which the tab
/// does as soon as the prompt is on its screen (`web/first-run.mjs`,
/// `watchShown`), so a person who hands the prompt to their agent at once can
/// beat that request when their machine draws the page slowly. The server
/// counts resolves that miss, so the asking is a few tries, not a wait.
async fn resolve(host: &dyn HttpClient, code: &str) -> Result<Resolved> {
    let began = std::time::Instant::now();
    loop {
        match login_code::resolve(host, code).await {
            Ok(resolved) => return Ok(resolved),
            Err(refusal) if refusal.code() == "unknown" && began.elapsed() < CODE_KEPT_WAIT => {
                tokio::time::sleep(CODE_KEPT_RETRY).await;
            }
            Err(refusal) => return Err(said_refusal(&refusal, code)),
        }
    }
}

/// How long a code the server does not know yet is asked about again, and how
/// often: a few tries inside the server's budget of misses (ten a minute from
/// one address), long enough for the tab of a machine that is drawing slowly
/// to keep the code it shows.
const CODE_KEPT_WAIT: std::time::Duration = std::time::Duration::from_secs(6);
const CODE_KEPT_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

/// What a refused code says to the person.
fn said_refusal(refusal: &login_code::CodeRefusal, code: &str) -> anyhow::Error {
    match refusal.code() {
        "unknown" => anyhow!(
            "Tilo couldn't use the code {code}: a code works once, for 10 minutes. \
             Ask the person for a new code from their Tilo tab (Set up with your agent)."
        ),
        "too-many" => {
            anyhow!("There were too many wrong codes from here: wait a minute, then try again.")
        }
        other => {
            debug_why(&format!("the code was refused ({other})"), refusal);
            anyhow!(
                "Tilo couldn't use this code: ask the person for a new code from their Tilo tab \
                 (Set up with your agent)."
            )
        }
    }
}

/// Tells the tab what this computer did with its code.
struct Reporter<'a> {
    host: &'a dyn HttpClient,
    report: String,
    agent: String,
}

impl Reporter<'_> {
    async fn step(&self, step: Step) {
        self.send(step, None).await;
    }

    /// The same, saying nothing when it does not reach the tab: a step a tab
    /// that has moved on is never told.
    async fn quiet_step(&self, step: Step) {
        let _ = login_code::report(self.host, &self.report(step, None)).await;
    }

    /// The tab shows `detail`, one sentence, as what failed.
    async fn failed(&self, detail: &str) {
        let sentence = detail.lines().next().unwrap_or(detail);
        self.send(Step::Failed, Some(sentence.chars().take(300).collect()))
            .await;
    }

    fn report(&self, step: Step, detail: Option<String>) -> Report {
        Report {
            report: self.report.clone(),
            step,
            agent: Some(self.agent.chars().take(protocol::MAX_AGENT_CHARS).collect()),
            detail,
        }
    }

    async fn send(&self, step: Step, detail: Option<String>) {
        if let Err(refusal) = login_code::report(self.host, &self.report(step, detail)).await {
            eprintln!("The person's tab was not told this step: {refusal}");
        }
    }
}

/// Claim a sign-in code for the person this device is a device of.
async fn claim(
    host: &dyn HttpClient,
    report: &str,
    key: &SigningKey,
    device: &Device,
) -> Result<Claimed> {
    let signed_at_ns = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    )?;
    login_code::claim(host, report, device.cert.clone(), key, signed_at_ns)
        .await
        .map_err(|refusal| match refusal.code() {
            "not-a-device" => anyhow!(
                "This computer is logged in to another Tilo account. Log it in again with a \
                 code from the account the person wants to use (in Tilo, choose \
                 \"Set up with your agent\")."
            ),
            other => {
                debug_why(&format!("the claim was refused ({other})"), &refusal);
                anyhow!(
                    "Tilo couldn't use this code: ask the person for a new one from their Tilo \
                     tab (Set up with your agent)."
                )
            }
        })
}

/// The name the server lists `workspace` under, when it lists it.
async fn workspace_name(host: &dyn HttpClient, workspace: &str) -> Option<String> {
    let path = format!(
        "{API_PREFIX}{}",
        tilo_server_protocol::directory::DIRECTORY_PATH
    );
    let (status, body) = host.post(&path, Vec::new()).await.ok()?;
    if status != 200 {
        return None;
    }
    let page: tilo_server_protocol::directory::DirectoryPage =
        serde_json::from_slice(&body).ok()?;
    page.workspaces
        .into_iter()
        .find(|listed| listed.workspace.eq_ignore_ascii_case(workspace))
        .map(|listed| listed.name)
        .filter(|name| !name.trim().is_empty())
}

/// What became of Tilo's tools for the agent.
#[derive(Debug, PartialEq, Eq)]
enum Tools {
    /// `tool` has them, in its own configuration, and they answer.
    Added(Tool),
    /// The agent adds them itself, where `tilo login` could not: what it
    /// prints says how.
    Yours,
}

/// How the person starts `tool` again with the conversation kept, since it
/// reads its servers only as it starts.
pub(crate) fn restart_of(tool: Tool) -> String {
    tool.restart().map_or_else(
        || format!("{} rereads its MCP servers as it runs.", tool.title()),
        |restart| format!("Restart it with this conversation kept: {restart}."),
    )
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

/// What `tilo login` prints last: what the agent does next, in order.
fn next_steps(agent: &Agent, tools: &Tools, tilo: &Path) -> String {
    let checks = format!(
        "Run `{} agent check`: it says whether Tilo's server is in you, and answers.",
        tilo.display()
    );
    // The tool's own words for starting again, when Tilo knows the tool but
    // could not write its configuration.
    let restart_of_tool = match tools {
        Tools::Yours => agent.tool.and_then(Tool::restart),
        Tools::Added(_) => None,
    };
    let mut steps: Vec<String> = Vec::new();
    match tools {
        Tools::Added(tool) => {
            if let Some(restart) = tool.restart() {
                steps.push(format!(
                    "Ask the person to restart {} with this conversation kept ({restart}): your \
                     Tilo tools load when it starts, and this conversation is still here after.",
                    agent.name
                ));
            }
            steps.push(checks);
        }
        Tools::Yours => {
            steps.push(format!(
                "Add Tilo's MCP server to yourself, as the server tilo: the stdio server {} with \
                 the argument mcp, or the streamable HTTP server {}. Put it where your own MCP \
                 configuration keeps servers, beside the ones already there, with your own \
                 tooling.",
                tilo.display(),
                agent::MCP_URL
            ));
            steps.push(format!(
                "{checks} If your configuration is a file, `{} agent check --config <its path>` \
                 has Tilo read it.",
                tilo.display()
            ));
            steps.push(restart_of_tool.map_or_else(
                || {
                    "If your tools load only when you start, ask the person to restart you with \
                     this conversation kept."
                        .to_owned()
                },
                |restart| {
                    format!(
                        "If your tools load only when you start, ask the person to restart you \
                         with this conversation kept ({restart})."
                    )
                },
            ));
        }
    }
    if let Tools::Added(tool) = tools
        && let Some(approval) = tool.approval()
    {
        steps.push(format!(
            "Let the server's tools run: {approval}, the way {} keeps servers it may use.",
            agent.name
        ));
    }
    // An editor runs other agents' prompts in its own terminal: the one
    // reading this may be another of them, and adds the server itself.
    if let Tools::Added(tool) = tools
        && tool.hosts_agents()
    {
        steps.push(format!(
            "If Tilo's tools are not in you — Tilo added them to {}'s configuration, and another \
             agent may be the one the person's prompt reached — add them to yourself the same \
             way: the server tilo, the stdio server {} with the argument mcp, or the streamable \
             HTTP server {}.",
            tool.title(),
            tilo.display(),
            agent::MCP_URL
        ));
    }
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

/// Add Tilo's tools to `agent`, saying what became of them.
fn add_tools(agent: &Agent, tilo: &Path, runner: &dyn agent::Runner) -> Tools {
    let Some(tool) = agent.tool else {
        return Tools::Yours;
    };
    if let agent::Registered::Failed(said) = agent::register(tool, tilo, runner) {
        println!(
            "Adding Tilo's tools to {} failed: {}",
            agent.name,
            said.lines().next().unwrap_or_default()
        );
    }
    // What the tool's own configuration holds is what counts: a command
    // saying it worked is not the same as the server being there, and
    // answering.
    match agent::verify(tool, runner) {
        agent::Configured::Yes {
            place, answered, ..
        } => {
            println!(
                "Added Tilo's tools to {}: the MCP server {} in {place}, answering as {}.",
                agent.name,
                agent::SERVER,
                answered.name
            );
            Tools::Added(tool)
        }
        agent::Configured::No { place } => {
            println!(
                "Tilo's tools are not in {} yet: {place} holds no server {}.",
                agent.name,
                agent::SERVER
            );
            Tools::Yours
        }
        agent::Configured::Unknown { why } => {
            println!("Tilo could not confirm its tools in {}: {why}.", agent.name);
            Tools::Yours
        }
    }
}

/// `tilo login <code> [--agent NAME] [--site ORIGIN]`.
pub fn login(code: &str, agent: Option<&str>, site: Option<&str>) -> Result<ExitCode> {
    let Some(code) = protocol::normalize_code(code) else {
        bail!(
            "{} is not a login code; one looks like K7Q-MX2P. Ask the person for the code their \
             Tilo tab shows (in Tilo, choose \"Set up with your agent\").",
            code.trim()
        );
    };
    trust_test_certificates()?;
    // A name of its own (`--agent "Reviewer"`) is another agent of the person's than the one
    // the tool goes by, run by the same tool: its tools are still added to it.
    let agent =
        agent
            .filter(|name| !name.trim().is_empty())
            .map_or_else(agent::detect_here, |name| {
                let named = agent::named(name);
                agent::Agent {
                    tool: named.tool.or_else(|| agent::detect_here().tool),
                    ..named
                }
            });
    let site = site_origin(site)?;
    let runtime = runtime()?;
    runtime.block_on(async {
        let host_origin = home_host(&Origin::new(&http(), &site)).await?;
        let host = Origin::new(&http(), &host_origin);
        let resolved = resolve(&host, &code).await?;
        let reporter = Reporter {
            host: &host,
            report: resolved.report.clone(),
            agent: agent.name.clone(),
        };
        reporter.step(Step::Resolved).await;
        let done = match (&resolved.link, resolved.sign_in) {
            (Some(link), _) => set_up(&reporter, link, &agent).await,
            (None, true) => vouch(&reporter, &host).await,
            (None, false) => Err(anyhow!(
                "Tilo couldn't use this code: ask the person for a new one from their Tilo tab \
                 (Set up with your agent)."
            )),
        };
        if let Err(error) = &done {
            debug_why("the login did not finish", error);
            // The tab reads one sentence: what happened, in plain words.
            reporter.failed(&upper_first(&error.to_string())).await;
        }
        done
    })
}

/// The device a sign-in made of this computer, as the login keeps it.
fn device_of(signed: &SignIn) -> Result<Device> {
    Ok(Device {
        site: signed.link.site.clone(),
        host_origin: signed.link.host_origin.clone(),
        workspace: signed.link.workspace_id(),
        workspace_name: None,
        user: hex::encode(signed.link.user_public_key),
        agent: signed.agent.clone(),
        label: signed.label.clone(),
        cert: hex::encode(postcard::to_allocvec(&signed.cert)?),
        host_cert: hex::encode(postcard::to_allocvec(&signed.host_cert)?),
    })
}

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Setting up: become a device of the person the link names, add Tilo's
/// tools to the agent, and enter the workspace.
async fn set_up(reporter: &Reporter<'_>, link: &str, agent: &Agent) -> Result<ExitCode> {
    let link = PairingLink::parse(link).map_err(|error| anyhow!(error))?;
    let dir = device_dir()?;
    let key = SigningKey::from_bytes(&rand::random());
    let host_web = Origin::new(&http(), &link.host_origin);
    eprintln!("Waiting for the person's tab to approve this computer…");
    let signed = certify(&host_web, &link, &agent.name, &key).await?;
    // The server names this computer: the name asked for, or the next numbered
    // one another device of the person reads by.
    let label = signed.label.clone();
    let mut device = device_of(&signed)?;
    device.workspace_name = workspace_name(&host_web, &device.workspace).await;
    store(&dir, &key, &device)?;
    // The tab follows this login; what the agent confirms afterwards is told
    // to it through what is kept here.
    let mut pending = Pending {
        report: reporter.report.clone(),
        host_origin: device.host_origin.clone(),
        tools_added: false,
        entered: false,
    };
    store_pending(&dir, &pending)?;
    reporter.step(Step::LoggedIn).await;
    let days = signed.cert.expires_at_ns.map_or(0, |end| {
        end.saturating_sub(signed.cert.issued_at_ns) / (24 * 3600 * 1_000_000_000)
    });
    println!("{}", logged_in_said(&label, days, &agent.name));
    let tilo = std::env::current_exe().context("finding this program")?;
    let tools = add_tools(agent, &tilo, &agent::System);
    if let Tools::Added(tool) = &tools {
        pending.tools_added = true;
        store_pending(&dir, &pending)?;
        reporter.step(Step::ToolsAdded).await;
        if tool.restart().is_some() {
            reporter.step(Step::Restart).await;
        }
    }
    let place = device.workspace_name.as_ref().map_or_else(
        || format!("the person's workspace on {}", host_of(&device.host_origin)),
        |name| {
            format!(
                "the person's workspace {name} on {}",
                host_of(&device.host_origin)
            )
        },
    );
    match connect_retrying(&key, &device).await {
        Ok(entered) => {
            println!("Entered {place}: their tab shows you there.");
            entered.close();
            // The workspace's one folder, on this computer too, asked for a
            // few times: it is the same server that just took this computer in.
            let mut folder =
                crate::folders::after_login(&device.workspace, device.workspace_name.as_deref());
            for pause in [1, 2] {
                if folder.is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(pause));
                folder = crate::folders::after_login(
                    &device.workspace,
                    device.workspace_name.as_deref(),
                );
            }
            match folder {
                Ok(folder) => println!(
                    "Its files are in {}, kept in sync both ways: anything written there is in the \
                     workspace, named for who wrote it (`tilo folders` lists the folders).",
                    folder.display()
                ),
                Err(error) => {
                    debug_why("putting the workspace's files in a folder", &error);
                    println!(
                        "Logged in. Its files aren't in a folder on this computer yet: \
                         `tilo folders` asks for it again."
                    );
                }
            }
            // An agent that adds Tilo's tools itself confirms them first: the
            // tab leaves only once they are in, and the agent's own check
            // tells it that they are.
            if matches!(tools, Tools::Added(_)) {
                reporter.step(Step::Entered).await;
            } else {
                pending.entered = true;
                store_pending(&dir, &pending)?;
            }
        }
        Err(error) => {
            // Said to the person's tab and to the agent alike: what did not
            // happen and the one action that fixes it.
            let said = enter_said(&error);
            reporter
                .failed(&format!(
                    "Logged in, but this computer can't open your workspace yet: {said}"
                ))
                .await;
            println!("Logged in, but this computer can't open {place} yet: {said}");
        }
    }
    println!();
    println!("{}", next_steps(agent, &tools, &tilo));
    Ok(ExitCode::SUCCESS)
}

/// Signing in: this computer, a device of the person, vouches for the tab
/// that asks, and the person's stand-in on the server approves it.
async fn vouch(reporter: &Reporter<'_>, host: &dyn HttpClient) -> Result<ExitCode> {
    let Some((key, device)) = load(&device_dir()?)? else {
        bail!(
            "This is a sign-in code, which needs a computer already logged in to Tilo, and this \
             one isn't. In Tilo, choose \"Set up with your agent\" instead."
        );
    };
    // The server tells the tab it approved, as it approves: that step is
    // never this computer's to report.
    claim(host, &reporter.report, &key, &device).await?;
    println!("Approved: your tab is signing in as you.");
    Ok(ExitCode::SUCCESS)
}

/// `tilo login` on a computer that holds the person's root itself: no tab and no code. The root
/// certifies a fresh device key for the agent here, the person's list of devices on the site's
/// server takes the key in, and the computer enters `workspace` (by name or id),
/// else their own workspace there, as [`set_up`] would after a tab approved it.
///
/// # Errors
/// The site or its server is not reached, the person has no workspace there by that name, or the
/// workspace refuses this device.
pub fn login_held(
    person: &crate::identity::Person,
    agent: Option<&str>,
    site: Option<&str>,
    workspace: Option<&str>,
) -> Result<ExitCode> {
    trust_test_certificates()?;
    let agent =
        agent
            .filter(|name| !name.trim().is_empty())
            .map_or_else(agent::detect_here, |name| {
                let named = agent::named(name);
                agent::Agent {
                    tool: named.tool.or_else(|| agent::detect_here().tool),
                    ..named
                }
            });
    let site = site_origin(site)?;
    let runtime = runtime()?;
    let (key, device) = runtime.block_on(held_device(person, &agent, &site, workspace))?;
    let dir = device_dir()?;
    store(&dir, &key, &device)?;
    println!(
        "Logged in: this computer holds {}'s identity, which made it \"{}\", one of their \
         devices.\n{}",
        person.name,
        device.label,
        which_agent(&agent.name)
    );
    let tilo = std::env::current_exe().context("finding this program")?;
    let tools = add_tools(&agent, &tilo, &agent::System);
    let place = format!(
        "{} on {}",
        device.workspace_name.as_deref().unwrap_or("the workspace"),
        host_of(&device.host_origin)
    );
    runtime.block_on(async {
        match connect_retrying(&key, &device).await {
            Ok(entered) => {
                println!("Entered {place}.");
                entered.close();
            }
            Err(error) => println!(
                "Logged in, but this computer can't open {place} yet: {}",
                enter_said(&error)
            ),
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
    println!("{}", next_steps(&agent, &tools, &tilo));
    Ok(ExitCode::SUCCESS)
}

/// The device key and the device entry of a login here: the site's server lets this device
/// connect, the person's list of devices takes its key in, and the device enters `workspace` by
/// name or id, else the person's own workspace there.
async fn held_device(
    person: &crate::identity::Person,
    agent: &agent::Agent,
    site: &str,
    workspace: Option<&str>,
) -> Result<(SigningKey, Device)> {
    let origin = home_host(&Origin::new(&http(), site)).await?;
    let server = Origin::new(&http(), &origin);
    let info = https::host_info(&server).await.map_err(|error| {
        debug_why("asking the site's server", &error);
        anyhow!(UNREACHED)
    })?;
    let dialed = info
        .address
        .as_ref()
        .map(tilo_server_client::Host::published)
        .ok_or_else(|| anyhow!("{origin} takes no device connections"))?;
    let key = SigningKey::from_bytes(&rand::random());
    let label = label(&agent.name, &hostname());
    let cert = held_device_cert(person, &key, &label)?;
    let user = person.user();
    let unentered = PairingLink::parse(&format!(
        "{site}/#/@{}/w/{}?pair={user}",
        host_of(&origin),
        uuid::Uuid::nil()
    ))
    .map_err(|error| anyhow!(error))?;
    let signed = SignIn {
        link: unentered,
        agent: agent.name.clone(),
        label: label.clone(),
        host: dialed,
        cert: cert.clone(),
        host_cert: info.host_cert.clone(),
    };
    let session = Entered::connect(&signed, Arc::new(key.clone()));
    let chosen = async {
        session
            .connected()
            .await
            .map_err(|refusal| anyhow!("{refusal}"))?;
        let mut list = session
            .personal_list(
                &tilo_server_session::workspaces::WorkspaceLabels::new(),
                None,
            )
            .await
            .map_err(|refusal| anyhow!("{refusal}"))?;
        // The person's list of devices takes this key in, as when a tab approves one.
        list.devices
            .push(tilo_server_session::workspaces::PersonalDevice {
                cert: cert.clone(),
                name: None,
                revoked: false,
                browser: None,
                agent: Some(agent.name.clone()),
                last_active_ms: None,
                online: false,
            });
        session
            .keep_personal(&list)
            .await
            .map_err(|refusal| anyhow!("{refusal}"))?;
        choose_workspace(&list.workspaces, workspace)
    }
    .await;
    session.close();
    let (id, name) = chosen?;
    let device = Device {
        site: format!("{site}/"),
        host_origin: origin,
        workspace: id,
        workspace_name: Some(name),
        user,
        agent: agent.name.clone(),
        label,
        cert: hex::encode(postcard::to_allocvec(&cert)?),
        host_cert: hex::encode(postcard::to_allocvec(&info.host_cert)?),
    };
    Ok((key, device))
}

/// A member device certificate for `key`, signed by the person's root.
fn held_device_cert(
    person: &crate::identity::Person,
    key: &SigningKey,
    label: &str,
) -> Result<tilo_user_identity::DeviceCert> {
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.root.verifying_key().to_bytes(),
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

/// The workspace a held login enters: the one `asked` names (its id, or its name in any case),
/// else the person's own one, else the only one they have.
fn choose_workspace(
    listed: &[tilo_server_session::workspaces::PersonalWorkspace],
    asked: Option<&str>,
) -> Result<(String, String)> {
    let pick = |found: &tilo_server_session::workspaces::PersonalWorkspace| {
        (found.workspace.clone(), found.name.clone())
    };
    if let Some(asked) = asked.map(str::trim).filter(|asked| !asked.is_empty()) {
        return listed
            .iter()
            .find(|found| {
                found.workspace.eq_ignore_ascii_case(asked)
                    || found.name.eq_ignore_ascii_case(asked)
            })
            .map(pick)
            .ok_or_else(|| {
                anyhow!(
                    "You have no workspace called \"{asked}\" there; {}",
                    names_of(listed)
                )
            });
    }
    let own: Vec<_> = listed
        .iter()
        .filter(|found| found.role.as_deref() == Some("owner"))
        .collect();
    match (own.as_slice(), listed) {
        ([only], _) => Ok(pick(only)),
        (_, [only]) => Ok(pick(only)),
        ([], []) => bail!("You have no workspace there yet: make one in Tilo first."),
        _ => bail!("Say which workspace with -w; {}", names_of(listed)),
    }
}

fn names_of(listed: &[tilo_server_session::workspaces::PersonalWorkspace]) -> String {
    let names: Vec<_> = listed
        .iter()
        .map(|found| format!("\"{}\"", found.name))
        .collect();
    format!("yours are {}.", names.join(", "))
}

pub(crate) fn host_of(origin: &str) -> &str {
    // A server on this computer is plain http, which a link names by its
    // address alone, as it names an https server by its name.
    origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
}

pub(crate) const NOT_LOGGED_IN: &str = "This computer is not logged in to Tilo. Ask the person to open tilo.sh, choose \"Set up with your agent\" and paste you the prompt it copies; it has you run tilo login with their code, like: tilo login K7Q-MX2P";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn device() -> Device {
        Device {
            site: "https://tilo.sh/".into(),
            host_origin: "https://eu1.tilo.sh".into(),
            workspace: "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0".into(),
            workspace_name: Some("Studio".into()),
            user: "ab".repeat(32),
            agent: "Claude Code".into(),
            label: "Claude Code on build-box".into(),
            cert: "00".into(),
            host_cert: "00".into(),
        }
    }

    tilo_test_env::scenario! {
        fn a_login_keeps_its_device_and_the_tab_it_came_from;

        phase the_device_is_kept_for_this_account_only_and_read_back;
        {
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
            // A key others can read is closed to them and used, not refused.
            std::fs::set_permissions(place.join("key"), std::fs::Permissions::from_mode(0o644))
                .unwrap();
            assert!(load(&place).unwrap().is_some());
            assert_eq!(
                std::fs::metadata(place.join("key"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            // A key that is not a key says what to do, not what it holds.
            std::fs::write(place.join("key"), b"short").unwrap();
            let said = load(&place).unwrap_err().to_string();
            assert_eq!(said, LOGIN_DAMAGED);
            assert!(load(&dir.path().join("none")).unwrap().is_none());
        }

        phase a_login_leaves_the_tab_for_the_agent_to_confirm;
        {
            let dir = tempfile::tempdir().unwrap();
            let pending = Pending {
                report: "aa".into(),
                host_origin: "https://eu1.tilo.sh".into(),
                tools_added: true,
                entered: false,
            };
            store_pending(dir.path(), &pending).unwrap();
            assert_eq!(load_pending(dir.path()), Some(pending));
            assert_eq!(load_pending(&dir.path().join("none")), None);
        }

        phase a_label_names_the_agent_and_the_computer_within_bounds;
        {
            assert_eq!(
                label("Claude Code", "build-box"),
                "Claude Code on build-box"
            );
        }
    }

    /// How a test's server answers a route and its body.
    type Answer = Box<dyn Fn(&str, &Value) -> (u16, Value) + Send + Sync>;

    /// A server that answers each route as a test says, and keeps what it
    /// was sent.
    struct FakeHost {
        answer: Answer,
        sent: Mutex<Vec<(String, Value)>>,
    }

    impl FakeHost {
        fn new(answer: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static) -> Self {
            Self {
                answer: Box::new(answer),
                sent: Mutex::new(Vec::new()),
            }
        }

        fn sent(&self) -> Vec<(String, Value)> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl HttpClient for FakeHost {
        fn post(&self, path: &str, body: Vec<u8>) -> tilo_server_client::https::HttpFuture<'_> {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let route = path.strip_prefix(API_PREFIX).unwrap_or(path).to_owned();
            let (status, answer) = (self.answer)(&route, &body);
            self.sent.lock().unwrap().push((route, body));
            Box::pin(async move { Ok((status, serde_json::to_vec(&answer).unwrap())) })
        }
    }

    #[tokio::test]
    async fn a_code_resolves_at_its_host_and_every_step_reaches_the_tab() {
        let host = FakeHost::new(|route, _| match route {
            protocol::RESOLVE_ROUTE => (
                200,
                json!({"link": "https://tilo.sh/#/@eu1.tilo.sh/w/x?pair=ab", "report": "aa"}),
            ),
            _ => (204, Value::Null),
        });
        let resolved = resolve(&host, "K7Q-MX2P").await.unwrap();
        assert!(!resolved.sign_in);
        assert_eq!(resolved.report, "aa");
        let reporter = Reporter {
            host: &host,
            report: resolved.report,
            agent: "Claude Code".into(),
        };
        reporter.step(Step::Resolved).await;
        reporter.step(Step::LoggedIn).await;
        reporter
            .failed("Entering the workspace failed: offline.\nmore detail")
            .await;
        let sent = host.sent();
        assert_eq!(
            sent[0],
            (
                protocol::RESOLVE_ROUTE.to_owned(),
                json!({"code": "K7Q-MX2P"})
            )
        );
        assert_eq!(
            sent[1..],
            [
                (
                    protocol::STEP_ROUTE.to_owned(),
                    json!({"report": "aa", "step": "resolved", "agent": "Claude Code"})
                ),
                (
                    protocol::STEP_ROUTE.to_owned(),
                    json!({"report": "aa", "step": "logged-in", "agent": "Claude Code"})
                ),
                (
                    protocol::STEP_ROUTE.to_owned(),
                    json!({"report": "aa", "step": "failed", "agent": "Claude Code",
                           "detail": "Entering the workspace failed: offline."})
                ),
            ]
        );
    }

    #[test]
    fn a_failed_login_says_what_to_do_without_the_machinery() {
        use tilo_server_protocol::auth::AuthError;
        use tilo_user_identity::DeviceCertError;
        let said = certify_said(&CertifyError::Expired);
        assert!(said.contains("Keep the tab open"), "{said}");
        for error in [
            CertifyError::Denied("declined".into()),
            CertifyError::Denied("not admissible".into()),
            CertifyError::Transport(LinkError::Unavailable("connect error".into())),
            CertifyError::Transport(LinkError::Refused("403".into())),
            CertifyError::Transport(LinkError::PeerVersion { theirs: 99 }),
            CertifyError::Transport(LinkError::HostChanged),
            CertifyError::Transport(LinkError::Sign("connect error".into())),
            CertifyError::Transport(LinkError::DeviceRevoked),
            CertifyError::Codec("bad frame".into()),
            CertifyError::Iroh("connect error".into()),
            CertifyError::Auth(AuthError::Revoked),
            CertifyError::Auth(AuthError::Cert(DeviceCertError::Expired)),
        ] {
            let said = certify_said(&error);
            for machinery in [
                "iroh:",
                "codec:",
                "auth:",
                "peer ",
                "protocol",
                "connect error",
                "bad frame",
                "not admissible",
                "403",
            ] {
                assert!(!said.contains(machinery), "{said} names {machinery}");
            }
            assert!(said.ends_with('.'), "{said}");
        }
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
    }

    #[tokio::test]
    async fn a_spent_or_guessed_code_says_what_to_do() {
        let spent = FakeHost::new(|_, _| (404, Value::Null));
        let said = resolve(&spent, "K7Q-MX2P").await.unwrap_err().to_string();
        assert!(said.contains("works once, for 10 minutes"), "{said}");
        let guessed = FakeHost::new(|_, _| (429, Value::Null));
        let said = resolve(&guessed, "K7Q-MX2P").await.unwrap_err().to_string();
        assert!(said.contains("too many wrong codes"), "{said}");
    }

    #[tokio::test]
    async fn a_device_claims_a_sign_in_code_with_its_own_signature() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let public = key.verifying_key();
        let host = FakeHost::new(move |route, body| {
            assert_eq!(route, protocol::CLAIM_ROUTE);
            let claim: protocol::Claim = serde_json::from_value(body.clone()).unwrap();
            let signature = ed25519_dalek::Signature::from_bytes(
                &hex::decode(&claim.signature).unwrap().try_into().unwrap(),
            );
            let message = protocol::claim_message(&claim.report, claim.signed_at_ns);
            if public.verify_strict(&message, &signature).is_ok() && claim.cert == "00" {
                (200, json!({"expires_at_ms": 5}))
            } else {
                (403, Value::Null)
            }
        });
        let claimed = claim(&host, "bb", &key, &device()).await.unwrap();
        assert_eq!(claimed.expires_at_ms, 5);
        let stranger = SigningKey::from_bytes(&[3; 32]);
        let said = claim(&host, "bb", &stranger, &device())
            .await
            .unwrap_err()
            .to_string();
        assert!(said.contains("another Tilo account"), "{said}");
    }

    #[tokio::test]
    async fn a_listed_workspace_is_named() {
        let host = FakeHost::new(|_, _| {
            (
                200,
                json!({"workspaces": [{"workspace": "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0", "name": "Lounge",
                    "defaultRole": "editor", "entry": "lounge/"}]}),
            )
        });
        assert_eq!(
            workspace_name(&host, "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0")
                .await
                .as_deref(),
            Some("Lounge")
        );
        assert_eq!(workspace_name(&host, "other").await, None);
    }

    tilo_test_env::scenario! {
        fn the_agent_is_told_its_next_steps;

        phase the_agent_is_told_what_to_do_next_in_its_own_terms;
        // A tool's own servers load only as it starts: the agent asks the person
        // to restart it, and says what it does after.
        {
            let tilo = Path::new("/home/me/.local/bin/tilo");
            let claude = agent::named("claude");
            let next = next_steps(&claude, &Tools::Added(Tool::Claude), tilo);
            let steps: Vec<&str> = next.lines().collect();
            assert_eq!(steps[0], "Next, in order:");
            assert_eq!(
                steps[1],
                "1. Ask the person to restart Claude Code with this conversation kept (/exit, then \
             `claude --continue`): your Tilo tools load when it starts, and this conversation is \
             still here after."
            );
            assert_eq!(
                steps[2],
                "2. Run `/home/me/.local/bin/tilo agent check`: it says whether Tilo's server is in \
             you, and answers."
            );
            assert!(steps[3].starts_with("3. Call the tilo_ls tool"), "{next}");
            assert!(
                steps[4].contains("\"path\": \"notes#open\"")
                    && steps[4].contains("\"title\": \"Hello from Claude Code\""),
                "the agent does one thing the person sees: {next}"
            );
        }

        phase an_agent_that_reads_its_servers_while_it_runs_is_not_told_to_restart;
        // A tool whose servers are there as it runs is not asked to restart; one
        // Tilo could not write for is told to add the server itself.
        {
            let tilo = Path::new("/home/me/.local/bin/tilo");
            let cursor = agent::named("cursor");
            let next = next_steps(&cursor, &Tools::Added(Tool::Cursor), tilo);
            assert!(!next.contains("restart"), "{next}");
            // Cursor holds a server's tools back until it is told to let it run.
            assert!(next.contains("run `agent mcp enable tilo`"), "{next}");
            // An editor's terminal runs other agents too: this one is told how to
            // add the server itself if the prompt reached it instead.
            assert!(
                next.contains("another agent may be the one the person's prompt reached"),
                "{next}"
            );
            assert!(next.contains("https://mcp.tilo.sh"), "{next}");
            // Claude Code is not such an editor: nothing hedges its steps.
            assert!(
                !next_steps(&agent::named("claude"), &Tools::Added(Tool::Claude), tilo)
                    .contains("another agent may be the one"),
                "only an editor's steps say another agent may be reading them"
            );
            assert!(
                next.contains("1. Run `/home/me/.local/bin/tilo agent check`"),
                "{next}"
            );
            let other = agent::named("Your agent");
            let next = next_steps(&other, &Tools::Yours, tilo);
            assert!(
                next.contains(
                    "1. Add Tilo's MCP server to yourself, as the server tilo: the stdio server \
                 /home/me/.local/bin/tilo with the argument mcp, or the streamable HTTP server \
                 https://mcp.tilo.sh"
                ),
                "{next}"
            );
            assert!(next.contains("agent check --config"), "{next}");
            assert!(next.contains("Call the tilo_ls tool"), "{next}");
        }

        phase the_check_tells_the_agent_what_it_does_first;
        // What the agent's own check says once the server is in: the one visible
        // thing it does for the person.
        {
            let said = first_act(&agent::named("codex"));
            assert!(said.starts_with("Start with the tilo_ls tool"), "{said}");
            assert!(said.contains("notes#new-note"), "{said}");
            assert!(said.contains("Hello from Codex"), "{said}");
        }
    }

    tilo_test_env::scenario! {
        fn a_login_code_is_read_and_resolved_at_its_site;

        phase codes_resolve_at_tilo_sh_unless_another_https_site_is_named;
        {
            assert_eq!(
                site_origin(Some("https://lounge.example/x")).unwrap(),
                "https://lounge.example"
            );
            assert!(site_origin(Some("http://lounge.example")).is_err());
            assert_eq!(DEFAULT_SITE, "https://tilo.sh");
        }

        phase a_login_code_reads_however_it_is_typed;
        {
            assert_eq!(
                protocol::normalize_code("k7q mx2p").as_deref(),
                Some("K7Q-MX2P")
            );
            assert!(login("https://tilo.sh/#/@eu1.tilo.sh/w/x?pair=ab", None, None).is_err());
        }
    }
}
