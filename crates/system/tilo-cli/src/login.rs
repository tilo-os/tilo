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
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_browser_peer::login::{self as peer_login, Door, SignIn};
use tilo_browser_peer::pairing_link::PairingLink;
use tilo_browser_peer::{HttpClient, PeerHandle};
use tilo_relay_client::https::Origin;
use tilo_relay_protocol::login_code::{self, Claim, Claimed, Report, Resolve, Resolved, Step};
use tilo_relay_protocol::web::API_PREFIX;

use crate::agent::{self, Agent, Registered, Tool};

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

/// Whether this computer logged in (`tilo login`).
pub fn logged_in() -> bool {
    device_dir().is_ok_and(|dir| dir.join("key").exists())
}

/// The site's door to the workspace the code names, as its `entry.json`
/// says (`home.admission`), when it names one for that host.
async fn door_of(site: &str, host_origin: &str) -> Option<(Origin, String)> {
    let (status, body) = Origin::new(&http(), site).get("/entry.json").await.ok()?;
    if status != 200 {
        return None;
    }
    let entry: Value = serde_json::from_slice(&body).ok()?;
    let (origin, path) = peer_login::site_door(&entry, host_origin)?;
    Some((Origin::new(&http(), &origin), path))
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
pub(crate) async fn connect(key: &SigningKey, device: &Device) -> Result<(PeerHandle, u64)> {
    let link = PairingLink::parse(&format!(
        "{}#/@{}/w/{}?pair={}",
        device.site,
        device.host_origin.trim_start_matches("https://"),
        device.workspace,
        device.user
    ))
    .map_err(|error| anyhow!(error))?;
    let host_web = Origin::new(&http(), &device.host_origin);
    let info = tilo_browser_peer::host_info(&host_web).await?;
    let endpoint = tilo_relay_client::https::host_endpoint(&info)
        .context("the workspace's host takes no device connections")?;
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

/// POST `body` as JSON to `route` below the host's API.
async fn post_json(
    host: &dyn HttpClient,
    route: &str,
    body: &(impl Serialize + Sync),
) -> Result<(u16, Vec<u8>)> {
    host.post(&format!("{API_PREFIX}{route}"), serde_json::to_vec(body)?)
        .await
        .map_err(|error| anyhow!("the host was not reached: {error}"))
}

/// What the code names, spending it.
async fn resolve(host: &dyn HttpClient, code: &str) -> Result<Resolved> {
    let (status, body) = post_json(
        host,
        login_code::RESOLVE_ROUTE,
        &Resolve {
            code: code.to_owned(),
        },
    )
    .await?;
    match status {
        200 => serde_json::from_slice(&body).context("the host answered something else"),
        404 => bail!(
            "the code {code} is unknown, used or expired: a code works once, for 10 minutes. \
             Ask the person to copy the prompt from their Tilo tab again."
        ),
        429 => bail!("too many wrong codes from here: wait a minute, then check the code"),
        other => bail!(
            "the host refused the code (HTTP {other}): {}",
            String::from_utf8_lossy(&body)
                .chars()
                .take(200)
                .collect::<String>()
        ),
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

    /// The tab shows `detail`, one sentence, as what failed.
    async fn failed(&self, detail: &str) {
        let sentence = detail.lines().next().unwrap_or(detail);
        self.send(Step::Failed, Some(sentence.chars().take(300).collect()))
            .await;
    }

    async fn send(&self, step: Step, detail: Option<String>) {
        let report = Report {
            report: self.report.clone(),
            step,
            agent: Some(
                self.agent
                    .chars()
                    .take(login_code::MAX_AGENT_CHARS)
                    .collect(),
            ),
            detail,
        };
        match post_json(self.host, login_code::STEP_ROUTE, &report).await {
            Ok((200 | 204, _)) => {}
            Ok((status, _)) => {
                eprintln!("The person's tab was not told this step (HTTP {status}).");
            }
            Err(error) => eprintln!("The person's tab was not told this step: {error:#}"),
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
    let signature = key.sign(&login_code::claim_message(report, signed_at_ns));
    let (status, body) = post_json(
        host,
        login_code::CLAIM_ROUTE,
        &Claim {
            report: report.to_owned(),
            cert: device.cert.clone(),
            signed_at_ns,
            signature: hex::encode(signature.to_bytes()),
        },
    )
    .await?;
    match status {
        200 => serde_json::from_slice(&body).context("the host answered something else"),
        403 => bail!(
            "this host signs nobody in for the person this computer is a device of: \
             sign in on the host where you set Tilo up"
        ),
        other => bail!(
            "the host refused the claim (HTTP {other}): {}",
            String::from_utf8_lossy(&body)
                .chars()
                .take(200)
                .collect::<String>()
        ),
    }
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
    /// `tool` has them, from its next start on.
    Added(Tool),
    /// The agent adds them itself.
    Yours,
}

/// What `tilo login` prints last: what the agent does next.
fn next_steps(agent: &Agent, tools: &Tools, tilo: &Path) -> String {
    let first = match tools {
        Tools::Added(tool) => format!(
            "1. Your Tilo tools load only after a restart: tell the person to restart you \
             (in {}: {}).",
            agent.name,
            tool.restart()
        ),
        Tools::Yours => format!(
            "1. Add Tilo's tools to yourself: the stdio MCP server named tilo, with the command \
             {} mcp",
            tilo.display()
        ),
    };
    format!(
        "Next:\n{first}\n2. Then call the tilo_ls tool, and tell the person what you can do \
         in Tilo: {WHAT_TILO_DOES}."
    )
}

/// Add Tilo's tools to `agent`, saying what became of them.
fn add_tools(agent: &Agent, tilo: &Path, runner: &dyn agent::Runner) -> Tools {
    let Some(tool) = agent.tool else {
        return Tools::Yours;
    };
    match agent::register(tool, tilo, runner) {
        Registered::Added | Registered::Already => {
            println!("Added Tilo's tools to {}: the MCP server tilo.", agent.name);
            Tools::Added(tool)
        }
        Registered::Missing => {
            println!(
                "{}'s own command is not on this PATH, so Tilo's tools are not added to it.",
                agent.name
            );
            Tools::Yours
        }
        Registered::Failed(said) => {
            println!(
                "Adding Tilo's tools to {} failed: {}",
                agent.name,
                said.lines().next().unwrap_or_default()
            );
            Tools::Yours
        }
    }
}

/// `tilo login <code> [--agent NAME] [--site ORIGIN]`.
pub fn login(code: &str, agent: Option<&str>, site: Option<&str>) -> Result<ExitCode> {
    let Some(code) = login_code::normalize_code(code) else {
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
    let signed = peer_login::sign_in(&host_web, &link, &agent.name, &label, &key, |_| {})
        .await
        .map_err(|error| match error {
            tilo_browser_peer::CertifyError::Expired => anyhow!(
                "the tab did not approve this computer in time: it approves only while it is open"
            ),
            tilo_browser_peer::CertifyError::Denied(reason) if reason.contains("declined") => {
                anyhow!("the tab declined this computer")
            }
            other => anyhow!("logging in failed: {other}"),
        })?;
    let mut device = Device::of(&signed)?;
    device.workspace_name = workspace_name(&host_web, &device.workspace).await;
    store(&dir, &key, &device)?;
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
    if matches!(tools, Tools::Added(_)) {
        reporter.step(Step::ToolsAdded).await;
        reporter.step(Step::Restart).await;
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
        Ok((peer, _)) => {
            reporter.step(Step::Entered).await;
            println!("Entered {place}: their tab shows you there.");
            peer.close();
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
    claim(host, &reporter.report, &key, &device).await?;
    reporter.step(Step::Approved).await;
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
            login_code::RESOLVE_ROUTE => (
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
                login_code::RESOLVE_ROUTE.to_owned(),
                json!({"code": "K7Q-MX2P"})
            )
        );
        assert_eq!(
            sent[1..],
            [
                (
                    login_code::STEP_ROUTE.to_owned(),
                    json!({"report": "aa", "step": "resolved", "agent": "Claude Code"})
                ),
                (
                    login_code::STEP_ROUTE.to_owned(),
                    json!({"report": "aa", "step": "logged-in", "agent": "Claude Code"})
                ),
                (
                    login_code::STEP_ROUTE.to_owned(),
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
            assert_eq!(route, login_code::CLAIM_ROUTE);
            let claim: Claim = serde_json::from_value(body.clone()).unwrap();
            let signature = ed25519_dalek::Signature::from_bytes(
                &hex::decode(&claim.signature).unwrap().try_into().unwrap(),
            );
            let message = login_code::claim_message(&claim.report, claim.signed_at_ns);
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

    #[test]
    fn the_agent_is_told_what_to_do_next_in_its_own_terms() {
        let tilo = Path::new("/home/me/.local/bin/tilo");
        let claude = agent::named("claude");
        assert_eq!(
            next_steps(&claude, &Tools::Added(Tool::Claude), tilo),
            "Next:\n1. Your Tilo tools load only after a restart: tell the person to restart you \
             (in Claude Code: /exit, then `claude --continue`).\n2. Then call the tilo_ls \
             tool, and tell the person what you can do in Tilo: you are in their Tilo workspace \
             as them, where they see you by name with what you do: tilo_ls lists its apps and \
             documents, tilo_read reads one, tilo_do runs their actions, and tilo_do with path \
             terminal#run runs a shell command there, all of which History keeps as your run."
        );
        let other = agent::named("Your agent");
        let next = next_steps(&other, &Tools::Yours, tilo);
        assert!(
            next.starts_with(
                "Next:\n1. Add Tilo's tools to yourself: the stdio MCP server named tilo, with \
                 the command /home/me/.local/bin/tilo mcp\n2. Then call the tilo_ls tool"
            ),
            "{next}"
        );
        assert!(!next.contains("restart"), "{next}");
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
            login_code::normalize_code("k7q mx2p").as_deref(),
            Some("K7Q-MX2P")
        );
        assert!(login("https://tilo.sh/#/@eu1.tilo.sh/w/x?pair=ab", None, None).is_err());
    }
}
