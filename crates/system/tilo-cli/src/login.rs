//! `tilo login <code>` and `tilo mcp`: this computer as one of the person's
//! devices, for the agent that runs here.
//!
//! The code is the short one the person's browser tab shows (`K7Q-MX2P`),
//! in the prompt it copies for their agent. `tilo login` resolves it at the
//! site's home host (`tilo_relay_protocol::login_code`), and tells that tab
//! each step as it does it:
//!
//! - **Setting up**: the code names the tab's pairing link. Logging in makes
//!   a device key here, asks the workspace's host to have the person's root
//!   (which only the tab holds) certify it, and keeps the key and its
//!   certificate in this account's state directory. No root is ever here.
//!   It adds Tilo's tools to the agent running it, and enters the workspace
//!   the link names, as the person: the tab shows the agent present there.
//! - **Signing in**: a tab with no identity yet asks a computer that is
//!   already a device of the person to vouch for it. This device claims the
//!   code for the person, and the person's stand-in on the host approves
//!   that tab.
//!
//! `tilo mcp` ([`crate::mcp`]) is the agent's MCP server over stdio, under
//! the name `tilo`, acting as that device.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_relay_client::CertifyError;
use tilo_relay_client::HttpClient;
use tilo_relay_client::https::{self, Origin};
use tilo_relay_client::login::{PairingLink, SignIn, door_at, sign_in};
use tilo_relay_client::login_code;
use tilo_relay_protocol::login_code::{self as protocol, Claimed, Report, Resolved, Step};
use tilo_relay_protocol::web::API_PREFIX;
use tilo_session_client::device::Device as Entered;

use crate::agent::{self, Agent, Tool};

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

/// The HTTPS client every request here goes through.
static CLIENT: OnceLock<tilo_net::Client> = OnceLock::new();

fn http() -> tilo_net::Client {
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

/// Where this account keeps the device: `$XDG_STATE_HOME/tilo/device`.
pub(crate) fn device_dir() -> Result<PathBuf> {
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
    /// Its name, when its host lists it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
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
            workspace_name: None,
            user: hex::encode(signed.link.user_public_key),
            agent: signed.agent.clone(),
            label: signed.label.clone(),
            cert: hex::encode(postcard::to_allocvec(&signed.cert)?),
            host_cert: hex::encode(postcard::to_allocvec(&signed.host_cert)?),
        })
    }

    pub(crate) fn certs(
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

/// What a login leaves behind for a later `tilo agent check`: the tab that
/// followed it, so the agent's own confirmation reaches that tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The tab's report, as its host knows it.
    pub report: String,
    /// The host that tab asks.
    pub host_origin: String,
    /// Whether that tab was told Tilo's tools are added.
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

/// Whether this computer logged in (`tilo login`).
pub fn logged_in() -> bool {
    device_dir().is_ok_and(|dir| dir.join("key").exists())
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
pub(crate) async fn connect(key: &SigningKey, device: &Device) -> Result<Entered> {
    let link = PairingLink::parse(&format!(
        "{}#/@{}/w/{}?pair={}",
        device.site,
        device.host_origin.trim_start_matches("https://"),
        device.workspace,
        device.user
    ))
    .map_err(|error| anyhow!(error))?;
    let host_web = Origin::new(&http(), &device.host_origin);
    let info = https::host_info(&host_web).await?;
    let host = info
        .address
        .as_ref()
        .map(tilo_relay_client::Host::published)
        .context("the workspace's host takes no device connections")?;
    let (cert, host_cert) = device.certs()?;
    let signed = SignIn {
        link,
        agent: device.agent.clone(),
        label: device.label.clone(),
        host,
        cert,
        host_cert,
    };
    let door = door_at(&Origin::new(&http(), &device.site), &device.host_origin).await;
    Ok(Entered::enter(door.as_ref(), &signed, Arc::new(key.clone())).await?)
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
    if url.scheme() != "https" {
        bail!("{named} is not an https:// site");
    }
    Ok(url.origin().ascii_serialization())
}

/// The site's home host, as its `entry.json` names it (`home.host.origin`).
async fn home_host(site: &Origin) -> Result<String> {
    let unreachable = || format!("{} names no Tilo host to log in at", site.name());
    let (status, body) = site
        .get("/entry.json")
        .await
        .map_err(|error| anyhow!("{} was not reached: {error}", site.name()))?;
    if status != 200 {
        bail!("{} (HTTP {status})", unreachable());
    }
    let entry: Value = serde_json::from_slice(&body).with_context(unreachable)?;
    let origin = entry
        .pointer("/home/host/origin")
        .and_then(Value::as_str)
        .with_context(unreachable)?;
    let url = tilo_net::Url::parse(origin).with_context(unreachable)?;
    if url.scheme() != "https" {
        bail!("{}: its host {origin} is not https", unreachable());
    }
    Ok(url.origin().ascii_serialization())
}

/// What the code names, spending it.
async fn resolve(host: &dyn HttpClient, code: &str) -> Result<Resolved> {
    login_code::resolve(host, code)
        .await
        .map_err(|refusal| match refusal.code() {
            "unknown" => anyhow!(
                "the code {code} is unknown, used or expired: a code works once, for 10 minutes. \
             Ask the person to copy the prompt from their Tilo tab again."
            ),
            "too-many" => {
                anyhow!("too many wrong codes from here: wait a minute, then check the code")
            }
            _ => anyhow!("the host refused the code: {refusal}"),
        })
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
                "this host signs nobody in for the person this computer is a device of: \
                 sign in on the host where you set Tilo up"
            ),
            _ => anyhow!("the host refused the claim: {refusal}"),
        })
}

/// The name the host lists `workspace` under, when it lists it.
async fn workspace_name(host: &dyn HttpClient, workspace: &str) -> Option<String> {
    let path = format!(
        "{API_PREFIX}{}",
        tilo_relay_protocol::directory::DIRECTORY_PATH
    );
    let (status, body) = host.post(&path, Vec::new()).await.ok()?;
    if status != 200 {
        return None;
    }
    let page: tilo_relay_protocol::directory::DirectoryPage = serde_json::from_slice(&body).ok()?;
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
        "Call the tilo_ls tool — that first call is what enters the person's workspace — then do \
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
        "Call the tilo_ls tool — that first call is what enters the person's workspace — and tell \
         them what you can do in Tilo: {WHAT_TILO_DOES}."
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
            "{} is not a login code; one looks like K7Q-MX2P. Ask the person to copy the prompt \
             from their Tilo tab again.",
            code.trim()
        );
    };
    trust_test_certificates()?;
    let agent = agent
        .filter(|name| !name.trim().is_empty())
        .map_or_else(agent::detect_here, agent::named);
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
            (None, false) => Err(anyhow!("the code names nothing this computer can do")),
        };
        if let Err(error) = &done {
            reporter.failed(&upper_first(&format!("{error:#}"))).await;
        }
        done
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
    let label = label(&agent.name, &hostname());
    let dir = device_dir()?;
    let key = SigningKey::from_bytes(&rand::random());
    let host_web = Origin::new(&http(), &link.host_origin);
    eprintln!("Waiting for the person's tab to approve this computer…");
    let signed = sign_in(&host_web, &link, &agent.name, &label, &key, |_| {})
        .await
        .map_err(|error| match error {
            CertifyError::Expired => anyhow!(
                "the tab did not approve this computer in time: it approves only while it is open"
            ),
            CertifyError::Denied(reason) if reason.contains("declined") => {
                anyhow!("the tab declined this computer")
            }
            other => anyhow!("logging in failed: {other}"),
        })?;
    let mut device = Device::of(&signed)?;
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
    println!(
        "Logged in: this computer is now \"{label}\", one of the person's devices for {days} days. \
         Their identity stays in their browser; only this device's own key is kept here."
    );
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
    match connect(&key, &device).await {
        Ok(entered) => {
            println!("Entered {place}: their tab shows you there.");
            entered.close();
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
            reporter
                .failed(&format!(
                    "Logged in, but entering the workspace failed: {error:#}"
                ))
                .await;
            println!("Entering {place} failed for now ({error:#}); tilo mcp tries again.");
        }
    }
    println!();
    println!("{}", next_steps(agent, &tools, &tilo));
    Ok(ExitCode::SUCCESS)
}

/// Signing in: this computer, a device of the person, vouches for the tab
/// that asks, and the person's stand-in on the host approves it.
async fn vouch(reporter: &Reporter<'_>, host: &dyn HttpClient) -> Result<ExitCode> {
    let Some((key, device)) = load(&device_dir()?)? else {
        bail!(
            "this is a sign-in code, for a computer already set up with Tilo, and this one is \
             not; on a new account, choose \"Set up with your agent\" in Tilo instead"
        );
    };
    // The host tells the tab it approved, as it approves: that step is
    // never this computer's to report.
    claim(host, &reporter.report, &key, &device).await?;
    println!("Approved: your tab is signing in as you.");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn host_of(origin: &str) -> &str {
    origin.trim_start_matches("https://").trim_end_matches('/')
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
    fn a_login_leaves_the_tab_for_the_agent_to_confirm() {
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

    #[test]
    fn a_label_names_the_agent_and_the_computer_within_bounds() {
        assert_eq!(
            label("Claude Code", "build-box"),
            "Claude Code on build-box"
        );
        assert_eq!(label(&"a".repeat(100), "x").chars().count(), 64);
    }

    /// How a test's host answers a route and its body.
    type Answer = Box<dyn Fn(&str, &Value) -> (u16, Value) + Send + Sync>;

    /// A host that answers each route as a test says, and keeps what it
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
        fn post(&self, path: &str, body: Vec<u8>) -> tilo_relay_client::https::HttpFuture<'_> {
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
        assert!(said.contains("signs nobody in"), "{said}");
    }

    #[tokio::test]
    async fn a_listed_workspace_is_named() {
        let host = FakeHost::new(|_, _| {
            (
                200,
                json!({"workspaces": [{"room": "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0", "name": "Lounge",
                    "defaultRole": "Editor", "door": "lounge/"}]}),
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

    /// A tool's own servers load only as it starts: the agent asks the person
    /// to restart it, and says what it does after.
    #[test]
    fn the_agent_is_told_what_to_do_next_in_its_own_terms() {
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

    /// A tool whose servers are there as it runs is not asked to restart; one
    /// Tilo could not write for is told to add the server itself.
    #[test]
    fn an_agent_that_reads_its_servers_while_it_runs_is_not_told_to_restart() {
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

    /// What the agent's own check says once the server is in: the one visible
    /// thing it does for the person.
    #[test]
    fn the_check_tells_the_agent_what_it_does_first() {
        let said = first_act(&agent::named("codex"));
        assert!(said.starts_with("Call the tilo_ls tool"), "{said}");
        assert!(said.contains("notes#new-note"), "{said}");
        assert!(said.contains("Hello from Codex"), "{said}");
    }

    #[test]
    fn codes_resolve_at_tilo_sh_unless_another_https_site_is_named() {
        assert_eq!(
            site_origin(Some("https://lounge.example/x")).unwrap(),
            "https://lounge.example"
        );
        assert!(site_origin(Some("http://lounge.example")).is_err());
        assert_eq!(DEFAULT_SITE, "https://tilo.sh");
    }

    #[test]
    fn a_login_code_reads_however_it_is_typed() {
        assert_eq!(
            protocol::normalize_code("k7q mx2p").as_deref(),
            Some("K7Q-MX2P")
        );
        assert!(login("https://tilo.sh/#/@eu1.tilo.sh/w/x?pair=ab", None, None).is_err());
    }
}
