//! `tilo-server workspace --persistent`: one always-on public workspace. The workspace's
//! owner root comes from the operator's encrypted secrets (mounted as a
//! file) and is kept in this host's secret store, never in a record, the
//! environment or a log. This is the documented exception to "the host
//! never holds a workspace's root": nobody is present to approve guests, and
//! the workspace is public anyway.
//!
//! Anyone who joins is their own person, with the role the workspace's
//! listing gives them, through the same entry as any public workspace
//! ([`Workspaces::publish`] and [`Workspaces::admit_listed`]); an unlisted
//! one takes people by invitation.

#![allow(
    clippy::similar_names,
    reason = "a workspace and its root are the two things this module is about"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use ed25519_dalek::SigningKey;
use tilo_server::daemon::ServerIdentity;
use tilo_server::web::WebHandle;
use tilo_server::workspaces::{WaitListed, Workspaces};
use tilo_server_protocol::directory::Listing;
use tilo_server_protocol::operator::{self, OperatorCommand, OperatorEvent, hex32};
use tilo_server_protocol::workspaces::Role;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::web_cli::{WorkspaceArgs, enrol, operator_lines, start_server_lending, web_config};
use tilo_server::entry;

mod bots;
mod discord_sign_in;
mod http;
use bots::{offer_sign_in, start_bots};

/// The secret store this host keeps workspace roots in.
pub(crate) const SECRET_APP: &str = "so.tilo.server";
/// How often unconfirmed addresses left at the entry are let go.
const WAIT_LIST_SWEEP_SECS: u64 = 60;
#[derive(clap::Args, Clone, Debug, Default)]
pub struct PersistentArgs {
    /// Run one persistent public workspace with this name instead of a
    /// short-lived one.
    #[arg(long, value_name = "NAME", requires_all = ["root_file", "workspace_id_file"])]
    pub persistent: Option<String>,
    /// The name people see for the persistent workspace; its operator name with
    /// a capital first letter when absent. A workspace already created under
    /// another name is renamed at start.
    #[arg(long = "workspace-name", value_name = "NAME", requires = "persistent")]
    pub workspace_name: Option<String>,
    /// A channel the workspace's Chat opens with while it has none, by name
    /// (lowercase letters, digits and `-`); repeatable, in order. Its id is
    /// `c_<name>` with `-` as `_`, the same on every start, so what maps
    /// onto it (a bridged Discord server's channels) finds it again.
    #[arg(
        long = "chat-channel",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::workspace_chat::channel_name
    )]
    pub chat_channels: Vec<String>,
    /// The channel, by name, the workspace's members report problems to: kept
    /// in the workspace's Chat on every start, after its other channels, since
    /// the site's pages send reports to it by its id (`c_<name>`).
    #[arg(
        long = "feedback-channel",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::workspace_chat::channel_name
    )]
    pub feedback_channel: Option<String>,
    /// A strip, by name, the workspace's shared desktop has in its first
    /// workspace; repeatable, in order. Added at start where missing; the
    /// strips people made are kept.
    #[arg(
        long = "strip",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::workspace_strips::strip_name
    )]
    pub strips: Vec<String>,
    /// A text published outside the workspace, announced so its readers find a
    /// discussion thread to arrive in: `<address>=<title>`, the address of
    /// the text and the title its thread is named; repeatable. A text whose
    /// thread the workspace keeps is left as it is.
    #[arg(
        long = "discussion",
        value_name = "ADDRESS=TITLE",
        requires = "persistent",
        value_parser = crate::workspace_discussions::parse
    )]
    pub discussions: Vec<crate::workspace_discussions::Published>,
    /// The workspace's owner root: a file of 64 hex characters (the Ed25519
    /// seed), mounted from the operator's encrypted secrets.
    #[arg(long = "root-file", value_name = "PATH")]
    pub root_file: Option<PathBuf>,
    /// Where the workspace's UUID is kept across restarts.
    #[arg(long = "workspace-id-file", value_name = "PATH")]
    pub workspace_id_file: Option<PathBuf>,
    /// Where the host's own endpoint (iroh, UDP) listens: devices of the
    /// operator that dial it, such as the members that take its workspaces'
    /// work, name this port. Without it the machine picks one at each start.
    #[arg(long = "iroh-listen", value_name = "ADDRESS", requires = "persistent")]
    pub iroh_listen: Option<std::net::SocketAddr>,
    /// Every host keeps workspaces of people's own (`workspace.create-own`) for
    /// the people this workspace's entry or an invitation into it let in, as
    /// many of them as a person asks for, sharing this many gigabytes per
    /// person: 10 when the flag is not given.
    #[arg(
        long = "person-storage-gb",
        value_name = "GB",
        default_value_t = 10,
        requires = "persistent"
    )]
    pub person_storage_gb: u64,
    /// List the workspace in the public directory with this description, so
    /// anyone may join it through its entry at `--default-role`. Asked once: the
    /// listing is then the workspace's own setting, and a start that names none
    /// leaves it as the workspace's people settled it.
    #[arg(
        long = "listing-description",
        value_name = "TEXT",
        requires = "persistent"
    )]
    pub listing_description: Option<String>,
    /// The role everyone who joins through the directory gets while the
    /// workspace is listed: `viewer` unless the host says otherwise.
    #[arg(
        long = "default-role",
        value_name = "ROLE",
        requires = "listing_description",
        value_parser = parse_role
    )]
    pub default_role: Option<Role>,
    /// Take people by invitation: an unlisted workspace's entry lets nobody new
    /// in, and members may invite someone with an invitation of their own,
    /// which they let in. Settled at every start. A listed workspace lets anyone
    /// in through its entry, so this goes with no `--listing-description`.
    #[arg(long = "members-only", conflicts_with = "listing_description")]
    pub members_only: bool,
    /// List the workspace, its entry admitting only people invited or
    /// pre-admitted: a member's invitation, or the one this entry issued a
    /// person it let in. Anyone else is told the workspace is joined by
    /// invitation instead of being handed one. Asked once: the entry is then
    /// the workspace's own setting. Goes with --listing-description, which
    /// --members-only cannot.
    #[arg(
        long = "invitation-only",
        requires = "listing_description",
        conflicts_with = "members_only"
    )]
    pub invitation_only: bool,
    /// How many requests left at the entry the workspace keeps while it takes
    /// people by invitation: each is an address (an email) someone who asked
    /// without an invitation leaves, on the workspace's wait list for whoever
    /// may invite until they take it off. `0`: the entry keeps none. It is the
    /// entry taking people by invitation that turns them away, so it is that
    /// entry this goes with, and the workspace holds it from then on.
    #[arg(
        long = "wait-list-max",
        value_name = "COUNT",
        default_value_t = 0,
        requires = "invitation_only"
    )]
    pub wait_list_max: usize,
    /// The web entry URL (a URL ending in `/`) the wait list's mails are
    /// sent from, which their links open: the confirmation link, and an
    /// invitation a member asks the entry to mail to a confirmed address. It
    /// defaults to the workspace's own web entry; without mail configured,
    /// the list keeps addresses and mails nothing.
    #[arg(
        long = "wait-list-mail",
        value_name = "URL",
        requires = "wait_list_max"
    )]
    pub wait_list_mail: Option<String>,
    /// The person the workspace is handed to at its start, by their user
    /// public key (64 hex characters): how a host gets its owner, who then
    /// lets everyone else in with `tilo workspace people add` and `tilo
    /// workspace people invite`. A workspace has one owner, so this takes
    /// effect while the host still owns it; once it is a person's, only they
    /// hand it on, and a start that names someone else says so in its log.
    #[arg(long = "owner", value_name = "USER_PUBLIC_KEY")]
    pub owners: Vec<String>,
    /// A UNIX socket the operator asks the running workspace through, one
    /// [`OperatorCommand`] line per connection: a deployment's workspace runs
    /// in a container, which has no stdin to write to. The answer is one
    /// [`OperatorEvent`] line on the same connection.
    #[arg(long = "operator-socket", value_name = "PATH")]
    pub operator_socket: Option<PathBuf>,
    /// The workspaces of other people this host holds whoever's they are, when
    /// an owner has the host governing one hand it over (`tilo workspace servers
    /// add`): `anyone`, or the workspaces' ids, comma-separated. Without it
    /// the host holds its own person's, and those of the people it let in.
    #[arg(long = "hold-for", value_name = "WORKSPACES")]
    pub hold_for: Option<String>,
    /// Let this server call addresses on its own network, not only the
    /// public internet: a model provider a person names, and the site of a
    /// server an owner has keep a workspace. Off on a public server, where an
    /// address someone names must not reach the network around it; on where
    /// the servers that work together share one network, as a stack on one
    /// machine does.
    #[arg(long = "call-own-network")]
    pub call_own_network: bool,
    /// Sign people in to `OpenAI` at this address in place of
    /// `https://auth.openai.com`: a local stack's fake, or an operator's own
    /// proxy in front of it.
    #[arg(long = "openai-sign-in", value_name = "URL")]
    pub openai_sign_in: Option<String>,
    /// Ask a signed-in `ChatGPT` plan at this address in place of
    /// `https://chatgpt.com/backend-api/codex`.
    #[arg(long = "chatgpt-backend", value_name = "URL")]
    pub chatgpt_backend: Option<String>,
    /// Use an API key issued at an `OpenAI` sign-in at this address in place
    /// of `https://api.openai.com/v1`.
    #[arg(long = "openai-api", value_name = "URL")]
    pub openai_api: Option<String>,
    /// An app authored into the workspace at its first start; repeatable.
    #[arg(long = "seed-app", value_name = "APP_ID")]
    pub seed_apps: Vec<String>,
    /// What to install at the workspace's first start, approved by the workspace's
    /// root, with its inputs (`<source> key=value …`); repeatable. The source
    /// is a bot directory entry, or a program file on this host, which the
    /// workspace runs under the program's own name.
    #[arg(long = "bot", value_name = "SPEC")]
    pub bot_specs: Vec<String>,
    /// The Discord bot token, mounted from the operator's encrypted
    /// secrets; kept where the Discord connector's worker asks for it.
    #[arg(long = "discord-token-file", value_name = "PATH")]
    pub discord_token_file: Option<PathBuf>,
    /// The Discord application members sign in with to link their
    /// account (OAuth 2, `identify`); with the redirect and consent page
    /// below, the workspace offers "Connect Discord". The workspace's host keeps the
    /// client secret and trades the code itself.
    #[arg(long = "discord-client-id", value_name = "ID", requires_all = ["discord_redirect_url", "discord_authorize_url", "discord_client_secret_file"])]
    pub discord_client_id: Option<String>,
    /// Where Discord sends the browser back: the workspace host's callback,
    /// which `--http` serves.
    #[arg(long = "discord-redirect-url", value_name = "URL")]
    pub discord_redirect_url: Option<String>,
    /// Discord's consent page.
    #[arg(long = "discord-authorize-url", value_name = "URL")]
    pub discord_authorize_url: Option<String>,
    /// The application's client secret, mounted from the operator's encrypted
    /// secrets. The callback trades the code for the account with it, and it
    /// never leaves this host.
    #[arg(
        long = "discord-client-secret-file",
        value_name = "PATH",
        requires = "discord_client_id"
    )]
    pub discord_client_secret_file: Option<PathBuf>,
    /// Where Discord's API is (`/api/v10` is appended): Discord's own host
    /// unless set. The Discord bridge's worker is pointed at it too, so a
    /// test server serves both the sign-in and the bridge.
    #[arg(long = "discord-api-base", value_name = "URL")]
    pub discord_api_base: Option<String>,
    /// The worker binary of a connector (`APP=PATH`), where it is not
    /// `<name>-worker` beside this binary; repeatable.
    #[arg(long = "connector-worker", value_name = "APP=PATH")]
    pub connector_workers: Vec<String>,
    /// The model API key the workspace's moderation agent asks with, mounted
    /// from the operator's encrypted secrets; kept in this host's secret
    /// store. Without it the workspace has no moderation agent and reports wait
    /// for its human Moderators. The same model looks over the reports its
    /// members write in the feedback channel before they send them, asked
    /// through the workspace, so their pages never hold the key.
    #[arg(long = "moderation-key-file", value_name = "PATH")]
    pub moderation_key_file: Option<PathBuf>,
    /// The model the moderation agent asks.
    #[arg(
        long = "moderation-model",
        value_name = "MODEL",
        requires = "moderation_key_file"
    )]
    pub moderation_model: Option<String>,
    /// Where avatars of authors a bot bridged out are published.
    #[arg(
        long = "public-avatars",
        value_name = "DIR",
        requires = "public_avatars_url"
    )]
    pub public_avatars: Option<PathBuf>,
    /// The public URL `--public-avatars` is served at.
    #[arg(long = "public-avatars-url", value_name = "URL")]
    pub public_avatars_url: Option<String>,
    /// Serve the deployment's plain-HTTP routes at this address: the health
    /// page (`/mode`), the published avatars (`/avatars/<name>`) and Discord's
    /// sign-in callback (`/discord/callback`), as an ingress publishes them.
    /// An address ending in `:0` takes a port the kernel picks, which the
    /// workspace announces once it serves.
    #[arg(long = "http", value_name = "ADDR", requires = "persistent")]
    pub http: Option<std::net::SocketAddr>,
    /// The largest file a member posts in the workspace's Chat that the workspace
    /// keeps once they left, in MiB; a larger one stays with its poster.
    #[arg(long = "attachment-max-mib", default_value_t = 25, value_parser = clap::value_parser!(u64).range(0..=64))]
    pub attachment_max_mib: u64,
    /// What one member's kept files may take together, in MiB; past it
    /// their oldest go first. The workspace takes in no more of one member's
    /// files in an hour either.
    #[arg(long = "attachment-member-mib", default_value_t = 250)]
    pub attachment_member_mib: u64,
    /// What all members' kept files may take together, in MiB; past it the
    /// oldest go first. `0` keeps none.
    #[arg(long = "attachment-workspace-mib", default_value_t = 2048)]
    pub attachment_room_mib: u64,
    /// Refound `APP/RECORD` from what the workspace can still read, at start,
    /// because its retained history lost the base its deltas were written
    /// over: the record is published again as the workspace's next checkpoint
    /// of it, and what its channel retained until now is archived beside the
    /// replay, never deleted. Repeatable; a record already refounded by an
    /// earlier start is not founded again.
    #[arg(long = "refound", value_name = "APP/RECORD", requires = "persistent")]
    pub refound: Vec<String>,
}

/// What every part of the persistent workspace works with.
pub(crate) struct PersistentWorkspace {
    pub workspaces: Workspaces,
    pub workspace: String,
    pub web: WebHandle,
}

impl PersistentWorkspace {
    /// The workspace's entry as its governance holds it: whether a directory
    /// lists it, whether it takes people by invitation, and how many requests
    /// it keeps.
    async fn entry_policy(&self) -> tilo_action_policy::EntryPolicy {
        self.workspaces
            .entry_policy(&self.workspace)
            .await
            .unwrap_or_default()
    }
}

/// The workspace name people see for the persistent workspace `name` (a lowercase
/// operator name such as `plaza`): its first letter capitalized.
pub(crate) fn workspace_name(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

pub(crate) fn read_root_file(path: &Path) -> anyhow::Result<SigningKey> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "persistent mode refuses to start without its workspace root ({})",
            path.display()
        )
    })?;
    let seed = hex32(text.trim())
        .context("the workspace root file must hold 64 lowercase hex characters")?;
    Ok(SigningKey::from_bytes(&seed))
}

pub(crate) fn keep_root(
    store: &tilo_secrets::SecretStore,
    workspace: &str,
    root: &SigningKey,
) -> anyhow::Result<()> {
    store
        .item("workspace-root", workspace)
        .put_blocking(&root.to_bytes())
        .context("keep the workspace root in the host's secret store")
}

/// Keep the mounted root for a workspace handed to this server under the id
/// it kept, checking it against the root already stored when there is one.
pub(crate) fn adopt_root(
    store: &tilo_secrets::SecretStore,
    workspace: &str,
    root: &SigningKey,
) -> anyhow::Result<()> {
    match store
        .item("workspace-root", workspace)
        .get_blocking()
        .context("read the workspace root from the host's secret store")?
    {
        Some(stored) => ensure!(
            stored.expose() == root.to_bytes(),
            "the mounted workspace root is not this workspace's owner"
        ),
        None => keep_root(store, workspace, root)?,
    }
    Ok(())
}

pub(crate) fn check_stored_root(
    store: &tilo_secrets::SecretStore,
    workspace: &str,
    root: &SigningKey,
) -> anyhow::Result<()> {
    let stored = store
        .item("workspace-root", workspace)
        .get_blocking()
        .context("read the workspace root from the host's secret store")?
        .context("this workspace's owner root is missing from the host's secret store")?;
    // A different mounted root would mint a different owner; refuse rather
    // than govern the workspace with a key it does not know.
    ensure!(
        stored.expose() == root.to_bytes(),
        "the mounted workspace root is not this workspace's owner"
    );
    Ok(())
}

pub(crate) fn newly_removed(
    seen: &mut BTreeSet<[u8; 32]>,
    members: &[tilo_peer_state::Member],
) -> Vec<[u8; 32]> {
    // Someone admitted again since is a member: a later removal is new.
    for member in members.iter().filter(|member| !member.removed) {
        if let Some(root) = member.root() {
            seen.remove(&root);
        }
    }
    members
        .iter()
        .filter_map(|member| {
            member
                .root()
                .filter(|root| member.removed && seen.insert(*root))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BotSpec {
    /// The directory entry it is installed from, or the program file.
    pub entry: String,
    pub inputs: BTreeMap<String, String>,
}

pub(crate) fn parse_bot_spec(text: &str) -> anyhow::Result<BotSpec> {
    let mut words = text.split_whitespace();
    let entry = words
        .next()
        .context("a bot spec names a directory entry or a program file")?
        .to_owned();
    let mut inputs = BTreeMap::new();
    for word in words {
        let (key, value) = word
            .split_once('=')
            .with_context(|| format!("bot input {word:?} is not key=value"))?;
        ensure!(
            inputs.insert(key.to_owned(), value.to_owned()).is_none(),
            "bot input {key} is given twice"
        );
    }
    Ok(BotSpec { entry, inputs })
}

/// A bot input as the program takes it: a JSON object or list as written
/// (`context={"link":"…"}`), anything else as text, so an id that looks
/// like a number stays the text it is.
fn input_value(text: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
        .unwrap_or_else(|| serde_json::Value::String(text.to_owned()))
}

/// `--default-role`: `viewer`, `commenter` or `editor`. A moderator is chosen,
/// never handed out at the entry.
fn parse_role(role: &str) -> Result<Role, String> {
    match Role::parse(role) {
        Some(role) if matches!(role, Role::Viewer | Role::Commenter | Role::Editor) => Ok(role),
        _ => Err(format!(
            "{role:?} is not a role people who join on their own may hold: say viewer, commenter or editor"
        )),
    }
}

/// The identity this workspace hosts as: the operator's root file, the host
/// device it keeps in its state, and the revocations its state's log holds.
/// `tilo device revoke` writes that log, so a device the root withdrew is
/// refused when it connects, as on the plain daemon.
fn hosted_identity(
    root: &SigningKey,
    host: SigningKey,
    server_cert: DeviceCert,
    server_state: &Path,
) -> anyhow::Result<ServerIdentity> {
    tilo_server::daemon::with_persisted_revocations(
        ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host,
            device_cert: server_cert,
            revocations: RevocationList::new(),
        },
        server_state,
    )
}

/// The host's own device key, kept beside the server's state. The root is
/// never written here.
fn host_key(state: &Path) -> anyhow::Result<SigningKey> {
    let path = state.join("host-device.key");
    match std::fs::read(&path) {
        Ok(bytes) => Ok(SigningKey::from_bytes(
            &<[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| anyhow::anyhow!("{} is corrupt", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = SigningKey::from_bytes(&rand::random());
            tilo_files::native::atomic::write_with_mode(&path, &key.to_bytes(), 0o600)
                .context("save the host device key")?;
            Ok(key)
        }
        Err(error) => Err(error).context("read the host device key"),
    }
}

/// The workspace id kept in `id_file`.
fn read_workspace_id(id_file: &Path) -> anyhow::Result<Option<String>> {
    fn read(path: &Path) -> anyhow::Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text.trim().to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }
    read(id_file)
}

/// The governed workspace: the one named in `workspace_id_file` (`kept`), else the only workspace
/// this server governs (a start interrupted between keeping the root and
/// writing the file), else a new one. The id file is written only once its
/// workspace exists, so an id this server holds nothing of names a workspace
/// founded on another device of its person: the server founds nothing under
/// it, and waits until one of the workspace's holders hands it over
/// (`workspace/servers#add`), as a server that joins any workspace is given it.
/// Founded here again, it would be a second founding of that workspace, which
/// its holders refuse.
async fn governed_workspace(
    workspaces: &Workspaces,
    shown: &str,
    kept: Option<String>,
    id_file: &Path,
    store: &tilo_secrets::SecretStore,
    root: &SigningKey,
) -> anyhow::Result<String> {
    let workspace = match kept {
        Some(workspace) => {
            if !holds(workspaces, &workspace).await {
                handed(workspaces, &workspace).await;
                adopt_root(store, &workspace, root)?;
            }
            workspace
        }
        None => match workspaces.list().await.as_slice() {
            [only] => only.workspace.clone(),
            [] => {
                // The root goes into the store before the workspace exists, so a
                // crash in between is found above, never orphaned.
                let created = workspaces.create(shown).await.map_err(anyhow::Error::msg)?;
                keep_root(store, &created.workspace, root)?;
                created.workspace
            }
            _ => {
                bail!("this server governs several workspaces; a persistent workspace governs one")
            }
        },
    };
    check_stored_root(store, &workspace, root)?;
    workspaces
        .rename(&workspace, shown)
        .await
        .map_err(|error| anyhow::anyhow!("name the workspace {shown:?}: {error}"))?;
    tilo_files::native::atomic::write_with_mode(id_file, workspace.as_bytes(), 0o644)
        .context("write the workspace id file")?;
    Ok(workspace)
}

/// Serve browsers on `listen`, the entry's wait list read from `wait_list`
/// once it is set.
async fn serve_browsers(
    server: &tilo_server::Server,
    args: &WorkspaceArgs,
    listen: std::net::SocketAddr,
    state: &Path,
    wait_list: &tilo_server::web::WaitListSlot,
) -> anyhow::Result<WebHandle> {
    let mut config = web_config(&args.web, listen, state);
    config.wait_list = Arc::clone(wait_list);
    server
        .serve_web_also_at(config, args.web.public_origins_also.clone())
        .await
        .context("start the browser listener")
}

/// Whether this server holds `workspace`.
async fn holds(workspaces: &Workspaces, workspace: &str) -> bool {
    workspaces
        .list()
        .await
        .iter()
        .any(|summary| summary.workspace == workspace)
}

/// Once one of `workspace`'s holders has handed it to this server. Until
/// then the server serves its hand-over and says it is starting.
async fn handed(workspaces: &Workspaces, workspace: &str) {
    tracing::info!(
        %workspace,
        "this server holds none of the workspace its id file names: it waits for one of the workspace's holders to hand it over (workspace/servers#add)"
    );
    while !holds(workspaces, workspace).await {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    tracing::info!(%workspace, "the workspace was handed over");
}

/// Whom the workspace has taken in as removed, so each removal is reported
/// once. Whom the entry keeps out is the workspace's own
/// ([`Workspaces::removed_within_the_day`]).
#[derive(Clone)]
struct Removals {
    state: Arc<std::sync::Mutex<RemovalState>>,
}

struct RemovalState {
    /// Every person accounted for as removed, so each removal is reported
    /// once.
    seen: BTreeSet<[u8; 32]>,
}

impl Removals {
    /// The removals of a workspace whose members are `members` now: everyone
    /// listed as removed is accounted for.
    fn load(members: &[tilo_peer_state::Member]) -> Self {
        let seen = members
            .iter()
            .filter(|member| member.removed)
            .filter_map(tilo_peer_state::Member::root)
            .collect();
        Self {
            state: Arc::new(std::sync::Mutex::new(RemovalState { seen })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RemovalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The people moderation removed since the last call, noted as removed
    /// now.
    fn taken_in(&self, members: &[tilo_peer_state::Member]) -> Vec<[u8; 32]> {
        newly_removed(&mut self.lock().seen, members)
    }
}

/// What an [`OperatorCommand::WaitList`] carries.
struct WaitListAsk {
    request: String,
    contact: String,
    removal_sha256: String,
    confirm_sha256: Option<String>,
    language: String,
}

/// Keep the request someone left at the entry without an invitation, while
/// the workspace takes people by invitation and keeps a wait list.
async fn answer_wait_list(context: &PersistentWorkspace, ask: WaitListAsk) -> OperatorEvent {
    let WaitListAsk {
        request,
        contact,
        removal_sha256,
        confirm_sha256,
        language,
    } = ask;
    let refused = |reason: String, full: bool| OperatorEvent::WaitListRefused {
        request: request.clone(),
        reason,
        full,
    };
    let policy = context.entry_policy().await;
    if !policy.keeps_requests() {
        return refused("the workspace keeps no wait list".into(), false);
    }
    let Some(removal) = hex32(&removal_sha256) else {
        return refused("the removal digest must be 64 hex characters".into(), false);
    };
    let confirm = match confirm_sha256.as_deref().map(hex32) {
        None => None,
        Some(Some(confirm)) => Some(confirm),
        Some(None) => {
            return refused(
                "the confirmation digest must be 64 hex characters".into(),
                false,
            );
        }
    };
    let secrets = tilo_server::workspaces::WaitListSecrets { removal, confirm };
    match context
        .workspaces
        .wait(
            &context.workspace,
            &contact,
            secrets,
            &language,
            policy.wait_list_max,
        )
        .await
    {
        Ok(WaitListed::Added { mail }) => OperatorEvent::WaitListed {
            request,
            added: true,
            mail,
        },
        Ok(WaitListed::Already { mail }) => OperatorEvent::WaitListed {
            request,
            added: false,
            mail,
        },
        Ok(WaitListed::Full) => refused("the wait list is full".into(), true),
        Err(error) => refused(error.to_string(), false),
    }
}

/// Take off the wait list the entry whose removal secret hashes to
/// `removal_sha256`, for whoever left it: answered alike whether or not
/// one did.
async fn answer_unlist(
    context: &PersistentWorkspace,
    request: String,
    removal_sha256: &str,
) -> OperatorEvent {
    let Some(removal) = hex32(removal_sha256) else {
        return OperatorEvent::WaitListFailed {
            request,
            reason: "the removal digest must be 64 hex characters".into(),
        };
    };
    match context
        .workspaces
        .leave_wait_list_for_operator(&context.workspace, removal)
        .await
    {
        Ok((told, contact)) => OperatorEvent::Unlisted {
            request,
            told: told.map(|mailbox| operator::WaitListMailbox {
                contact: mailbox.contact,
                language: mailbox.language,
            }),
            contact_sha256: contact.map(hex::encode),
        },
        Err(error) => OperatorEvent::WaitListFailed {
            request,
            reason: error.to_string(),
        },
    }
}

/// Confirm the entry whose confirmation secret hashes to `confirm_sha256`.
async fn answer_confirm(
    context: &PersistentWorkspace,
    request: String,
    confirm_sha256: &str,
) -> OperatorEvent {
    let Some(confirm) = hex32(confirm_sha256) else {
        return OperatorEvent::WaitListFailed {
            request,
            reason: "the confirmation digest must be 64 hex characters".into(),
        };
    };
    match context
        .workspaces
        .confirm_wait_list(&context.workspace, confirm)
        .await
    {
        Ok(confirmed) => OperatorEvent::Confirmed { request, confirmed },
        Err(error) => OperatorEvent::WaitListFailed {
            request,
            reason: error.to_string(),
        },
    }
}

/// Where the entry whose removal secret hashes to `removal_sha256` stands.
async fn answer_state(
    workspaces: &Workspaces,
    workspace: &str,
    request: String,
    removal_sha256: &str,
) -> OperatorEvent {
    let Some(removal) = hex32(removal_sha256) else {
        return OperatorEvent::WaitListFailed {
            request,
            reason: "the removal digest must be 64 hex characters".into(),
        };
    };
    match workspaces.wait_list_state(workspace, removal).await {
        Ok(state) => OperatorEvent::Status { request, state },
        Err(error) => OperatorEvent::WaitListFailed {
            request,
            reason: error.to_string(),
        },
    }
}

/// The answer to a command about the wait list.
async fn answer_wait_list_command(
    context: &PersistentWorkspace,
    command: OperatorCommand,
) -> OperatorEvent {
    match command {
        OperatorCommand::WaitList {
            request,
            contact,
            removal_sha256,
            confirm_sha256,
            language,
        } => {
            let ask = WaitListAsk {
                request,
                contact,
                removal_sha256,
                confirm_sha256,
                language,
            };
            answer_wait_list(context, ask).await
        }
        OperatorCommand::Unlist {
            request,
            removal_sha256,
        } => answer_unlist(context, request, &removal_sha256).await,
        OperatorCommand::Confirm {
            request,
            confirm_sha256,
        } => answer_confirm(context, request, &confirm_sha256).await,
        OperatorCommand::Status {
            request,
            removal_sha256,
        } => {
            answer_state(
                &context.workspaces,
                &context.workspace,
                request,
                &removal_sha256,
            )
            .await
        }
        _ => OperatorEvent::WaitListFailed {
            request: String::new(),
            reason: "not a wait-list command".into(),
        },
    }
}

/// The work setting `--hold-for` names: its own person's work, and the
/// workspaces of others it names.
fn hold_for(named: Option<&str>) -> anyhow::Result<tilo_server_protocol::WorkSetting> {
    let mut work = tilo_server_protocol::WorkSetting::own_person();
    match named.map(str::trim) {
        None | Some("") => {}
        Some("anyone") => work.from = tilo_server_protocol::WorkFrom::Anyone,
        Some(ids) => {
            work.from = tilo_server_protocol::WorkFrom::Workspaces(
                ids.split(',')
                    .map(|id| {
                        tilo_server_protocol::WorkspaceId::parse(id.trim())
                            .with_context(|| format!("--hold-for: {id:?} is no workspace id"))
                    })
                    .collect::<anyhow::Result<_>>()?,
            );
        }
    }
    Ok(work)
}

/// Hand the workspace to the first of `owners` (their user roots, hex) while
/// this host still owns it: its governance names them its owner from then on,
/// so the entry lets them in even when it takes people by invitation, and a
/// key of theirs lists the workspace, enters it and brings others in before
/// any device of theirs has joined. A workspace a person owns already stays
/// theirs; a deployment naming someone else is told so in the log.
async fn settle_owners(
    workspaces: &Workspaces,
    workspace: &str,
    owners: &[String],
) -> anyhow::Result<()> {
    for key in owners {
        let user = hex32(key.trim())
            .with_context(|| format!("--owner {key:?}: a user public key is 64 hex characters"))?;
        if workspaces.is_owner(workspace, user).await {
            continue;
        }
        if workspaces.has_owner(workspace).await {
            tracing::warn!(%workspace, person = %key,
                "the workspace has its owner already: only they hand it to someone else");
            continue;
        }
        workspaces
            .declare_owner(workspace, user, None)
            .await
            .map_err(|error| anyhow::anyhow!("hand the workspace to {key}: {error}"))?;
        tracing::info!(%workspace, person = %key, "the deployment's owner owns the workspace");
    }
    Ok(())
}

/// The record in the workspace itself that says what its hosts settled in it
/// on the operator's behalf: the apps seeded (`seeded`) and the records
/// refounded (`refounded`), each a map from its name to a note. Being the
/// workspace's, it moves with the workspace: any host serving it reads what
/// was done, and a host started on an empty directory seeds nothing again.
pub(crate) const SETTLED: (&str, &str) = ("so.tilo.server", "settled");

/// What the workspace's [`SETTLED`] record names under `kind`.
pub(crate) async fn settled_in(
    workspaces: &Workspaces,
    workspace: &str,
    kind: &'static str,
) -> anyhow::Result<BTreeSet<String>> {
    use automerge::ReadDoc as _;
    let Some(bytes) = workspaces
        .resource_document(workspace, SETTLED.0, SETTLED.1)
        .await
        .map_err(|error| anyhow::anyhow!("read what the workspace settled: {error}"))?
    else {
        return Ok(BTreeSet::new());
    };
    let doc = automerge::AutoCommit::load(&bytes).context("read what the workspace settled")?;
    Ok(match doc.get(automerge::ROOT, kind) {
        Ok(Some((automerge::Value::Object(_), map))) => doc.keys(&map).collect(),
        _ => BTreeSet::new(),
    })
}

/// Note in the workspace's [`SETTLED`] record that `name` was settled under
/// `kind`.
async fn mark_settled(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    kind: &'static str,
    name: &str,
    note: &str,
) -> anyhow::Result<()> {
    workspaces
        .author_workspace_document(workspace, cert, SETTLED, |doc| {
            use automerge::ReadDoc as _;
            use automerge::transaction::Transactable as _;
            let map = match doc.get(automerge::ROOT, kind) {
                Ok(Some((automerge::Value::Object(automerge::ObjType::Map), map))) => map,
                _ => doc
                    .put_object(automerge::ROOT, kind, automerge::ObjType::Map)
                    .map_err(|error| error.to_string())?,
            };
            doc.put(&map, name, note).map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| anyhow::anyhow!("note {name} as settled: {error}"))
}

/// Seed the workspace with `apps`: each one's record written from its own
/// factory, as the workspace's governor, and noted in the workspace's
/// [`SETTLED`] record so a later start, on this host or another serving it,
/// seeds only what it added since.
///
/// A workspace the operator just created holds no channel yet, and its records
/// are sealed from their first write, so each seed's genesis waits for the
/// group the host's channel service creates for it: the wait is bounded by the
/// service (`crate::workspace_keys::OPEN_WAIT`, longer than the group may take to be
/// founded), and each app is named in the error when its keys never come.
pub(crate) async fn seed_apps(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    apps: &[String],
) -> anyhow::Result<()> {
    let seeded = settled_in(workspaces, workspace, "seeded").await?;
    let catalogue = tilo_agent_session::AppCatalogue::native();
    for app in apps.iter().filter(|app| !seeded.contains(*app)) {
        let record = tilo_workspace_service::agents::workspace_resource(&catalogue, app);
        workspaces
            .prepare_application(workspace, cert, &catalogue, app, &record)
            .await
            .map_err(|error| anyhow::anyhow!("seed {app}: {error}"))?;
        mark_settled(workspaces, workspace, cert, "seeded", app, "seeded").await?;
    }
    Ok(())
}

/// What the workspace keeps of the files members post in its Chat; `None`
/// when it keeps none.
fn attachment_limits(
    persistent: &PersistentArgs,
) -> Option<tilo_server::workspace_bots::attachments::AttachmentLimits> {
    const MIB: u64 = 1024 * 1024;
    (persistent.attachment_room_mib > 0).then(|| {
        tilo_server::workspace_bots::attachments::AttachmentLimits {
            file_bytes: persistent.attachment_max_mib.saturating_mul(MIB),
            member_bytes: persistent.attachment_member_mib.saturating_mul(MIB),
            workspace_bytes: persistent.attachment_room_mib.saturating_mul(MIB),
        }
    })
}

/// [`attachment_limits`] as the workspace's Chat reads them.
fn workspace_files(persistent: &PersistentArgs) -> chat::chat::WorkspaceFiles {
    attachment_limits(persistent).map_or_else(Default::default, |limits| {
        chat::chat::WorkspaceFiles {
            largest: limits.file_bytes,
            member_hourly: limits.member_bytes,
        }
    })
}

/// Give the workspace's chat, when it is seeded and while it has only the
/// channels every workspace opens with, the operator's starting `channels`, and
/// tell it what the workspace keeps of members' `files`. What people wrote is
/// never touched, and a chat with channels keeps exactly those.
pub(crate) async fn settle_chat(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    apps: &[String],
    channels: &[String],
    feedback: Option<&str>,
    files: chat::chat::WorkspaceFiles,
    discussions: &[crate::workspace_discussions::Published],
) -> anyhow::Result<()> {
    if !apps
        .iter()
        .any(|app| app == crate::workspace_chat::CHAT_APP)
    {
        return Ok(());
    }
    let catalogue = tilo_agent_session::AppCatalogue::native();
    let record = tilo_workspace_service::agents::workspace_resource(
        &catalogue,
        crate::workspace_chat::CHAT_APP,
    );
    workspaces
        .author_application(
            workspace,
            cert,
            &catalogue,
            crate::workspace_chat::CHAT_APP,
            &record,
            |doc| {
                crate::workspace_chat::settle_chat(doc, channels)?;
                if let Some(name) = feedback {
                    crate::workspace_chat::settle_feedback_channel(doc, name)?;
                }
                crate::workspace_chat::settle_files(doc, files)?;
                crate::workspace_discussions::settle_threads(doc, discussions).map(drop)
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("open the chat's channels: {error}"))
}

/// Give the workspace's shared desktop the operator's `strips` where missing.
pub(crate) async fn settle_strips(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    strips: &[String],
) -> anyhow::Result<()> {
    if strips.is_empty() {
        return Ok(());
    }
    workspaces
        .author_workspace_document(workspace, cert, crate::workspace_strips::DESKTOP, |doc| {
            crate::workspace_strips::settle_strips(doc, workspace, strips)
        })
        .await
        .map(drop)
        .map_err(|error| anyhow::anyhow!("give the shared desktop its strips: {error}"))
}

/// Refound each `APP/RECORD` the operator named, once: a record whose
/// retained history lost the base its deltas were written over is published
/// again as the workspace's next checkpoint of it, and what its channel
/// retained until now is archived beside the replay. What was done is noted in
/// the workspace's [`SETTLED`] record, so a later start naming the same record,
/// on any host serving the workspace, does not found it again.
async fn refound_records(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    named: &[String],
) -> anyhow::Result<()> {
    if named.is_empty() {
        return Ok(());
    }
    let done = settled_in(workspaces, workspace, "refounded").await?;
    for entry in named {
        let (app, record) = entry
            .split_once('/')
            .with_context(|| format!("--refound {entry:?} is not APP/RECORD"))?;
        if done.contains(entry) {
            continue;
        }
        let outcome = workspaces
            .refound_document(workspace, app, record)
            .await
            .map_err(|error| anyhow::anyhow!("refound {entry}: {error}"))?;
        tracing::info!(
            workspace = %workspace,
            app = %outcome.app,
            record = %outcome.record,
            "a record was refounded"
        );
        let note = "refounded".to_owned();
        mark_settled(workspaces, workspace, cert, "refounded", entry, &note).await?;
    }
    Ok(())
}

/// A workspace the operator lists publicly, and governs, gets its moderation
/// bot's grant once, when the operator's setup first brings it up: the
/// operator, as its own person, grants the reading and undoable changes in
/// Chat the bot moderates by. The decision is noted in the workspace's own
/// settlement document, so a restart neither grants it again nor takes back a
/// person's later choice: after that, the switch in the workspace's settings
/// is the only way to change it. A workspace the operator does not list, or
/// whose applications another of its holders sets up
/// (`Workspaces::governs_applications`), is left alone: the grant is part of
/// that setup, and a copy just handed over may not read what that holder
/// settled yet, nor be named a holder that writes it.
pub(crate) async fn settle_moderation(
    workspaces: &Workspaces,
    workspace: &str,
    persistent: &PersistentArgs,
) -> anyhow::Result<()> {
    if persistent.listing_description.is_none()
        || !workspaces
            .governs_applications(workspace)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?
    {
        return Ok(());
    }
    let grant = tilo_action_policy::bot::MODERATION_GRANT;
    if !settled_in(workspaces, workspace, "moderation")
        .await?
        .contains(grant)
    {
        if !tilo_server::workspace_bots::enabled(workspaces, workspace)
            .await
            .map_err(|error| anyhow::anyhow!("read the workspace's moderation grant: {error}"))?
        {
            tilo_server::workspace_bots::enable(workspaces, workspace)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("enable the workspace's moderation bot: {error}")
                })?;
        }
        mark_settled(
            workspaces,
            workspace,
            workspaces.serving_certificate(),
            "moderation",
            grant,
            "granted at start",
        )
        .await?;
    }
    // Said wherever the grant is present, so a fresh stack's check reads the
    // same line after a restart; a workspace whose person turned it off says
    // nothing.
    if tilo_server::workspace_bots::enabled(workspaces, workspace)
        .await
        .map_err(|error| anyhow::anyhow!("read the workspace's moderation grant: {error}"))?
    {
        tracing::info!(
            workspace,
            "the moderation bot is enabled for this workspace"
        );
    }
    Ok(())
}

pub(crate) async fn ensure_guest_policy(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
) -> anyhow::Result<()> {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, Role, TreePath};
    // Guests are Editors; in a public workspace they must not trigger outside
    // effects from any app (sending mail, calling services).
    let deny = Grant::new(
        "public-workspace-guests-no-outside",
        Grantee::role(Role::Editor),
        TreePath::workspace(workspace),
        KindSet::OUTSIDE,
        Answer::Deny,
        Grantee::person(hex::encode(cert.user_public_key)),
    );
    // The request is the grant as this build writes it: the same grant settled
    // again is the request done before, and a grant written differently (its
    // answer, or the encoding a build gives it) is a new request that replaces
    // the one held under its id, never one the host refuses as an ID reused.
    let written = serde_json::to_vec(&deny).context("the guests' grant as written")?;
    let request_id = *blake3::Hasher::new()
        .update(b"public-workspace-guests-no-outside\0")
        .update(&written)
        .finalize()
        .as_bytes();
    workspaces
        .put_workspace_grants(workspace, cert, request_id, vec![deny])
        .await
        .map_err(|error| anyhow::anyhow!("deny outside effects to guests: {error}"))?;
    Ok(())
}

/// The grant letting the workspace's members (guests are Editors) invite
/// someone, which a workspace taking people by invitation holds and an open one
/// does not: anyone joins an open workspace through its entry.
const MEMBERS_INVITE: &str = "members-invite";

/// Let the workspace's members invite people, and let in whom they invited,
/// while the workspace takes people by invitation (`members_only`); take that
/// back once it lets anyone in again. Settled at every start.
pub(crate) async fn settle_member_invites(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    members_only: bool,
) -> anyhow::Result<()> {
    use tilo_action_policy::{Answer, Grant, GrantId, Grantee, KindSet, Role, roles};
    // One request per start and setting: the same setting settled again
    // after the workspace changed writes it again, which changes nothing.
    let generation = workspaces
        .summary(workspace)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .generation;
    let request_id =
        *blake3::hash(format!("{MEMBERS_INVITE}/{members_only}/{generation}").as_bytes())
            .as_bytes();
    if members_only {
        let invite = Grant::new(
            MEMBERS_INVITE,
            Grantee::role(Role::Editor),
            roles::governance_action(workspace, roles::governance::INVITE),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person(hex::encode(cert.user_public_key)),
        );
        workspaces
            .put_workspace_grants(workspace, cert, request_id, vec![invite])
            .await
            .map_err(|error| anyhow::anyhow!("let members invite: {error}"))?;
    } else {
        workspaces
            .remove_workspace_grants(
                workspace,
                cert,
                request_id,
                vec![GrantId(MEMBERS_INVITE.into())],
            )
            .await
            .map_err(|error| anyhow::anyhow!("take back members inviting: {error}"))?;
    }
    Ok(())
}

/// Say a setting of the operator's that did not settle at this start: it is
/// settled again at the next one.
fn settled(workspace: &str, result: anyhow::Result<()>) {
    if let Err(error) = result {
        tracing::error!(
            workspace,
            error = format!("{error:#}"),
            "a setting of the workspace did not settle; the host serves it as it was and tries again at the next start"
        );
    }
}

/// List the workspace in the public directory as the operator configured it,
/// and take people by invitation only if it says so. The operator's word is
/// asked once: the listing and the entry are the workspace's governance from
/// then on, so an operator that names neither leaves them as the workspace's
/// own people settled them, and a restart changes nothing.
async fn settle_listing(
    workspaces: &Workspaces,
    workspace: &str,
    server_cert: &DeviceCert,
    persistent: &PersistentArgs,
) -> anyhow::Result<()> {
    let Some(description) = &persistent.listing_description else {
        return Ok(());
    };
    workspaces
        .publish(
            workspace,
            server_cert,
            Listing {
                description: description.clone(),
                default_role: persistent.default_role.unwrap_or(Role::Viewer),
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("list the workspace in the directory: {error}"))?;
    Ok(())
}

/// Take people by invitation only, and keep the wait list the operator sized,
/// when it says so.
async fn settle_invitation_only(
    workspaces: &Workspaces,
    workspace: &str,
    server_cert: &DeviceCert,
    persistent: &PersistentArgs,
) -> anyhow::Result<()> {
    if !persistent.invitation_only {
        return Ok(());
    }
    workspaces
        .list_invitation_only(workspace, server_cert, true)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    workspaces
        .list_wait_list_max(workspace, server_cert, persistent.wait_list_max)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one startup sequence; the hosted server lives until shutdown"
)]
pub(crate) async fn run(
    args: WorkspaceArgs,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let persistent = args.persistent.clone();
    let name = persistent
        .persistent
        .clone()
        .context("--persistent names the workspace")?;
    let root = read_root_file(persistent.root_file.as_deref().context("--root-file")?)?;
    let id_file = persistent
        .workspace_id_file
        .clone()
        .context("--workspace-id-file")?;
    let bots = persistent
        .bot_specs
        .iter()
        .map(|spec| parse_bot_spec(spec))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let workers = persistent
        .connector_workers
        .iter()
        .map(|named| {
            let (app, path) = named
                .split_once('=')
                .with_context(|| format!("--connector-worker {named:?} is not APP=PATH"))?;
            Ok((app.to_owned(), PathBuf::from(path)))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let state = args.state.clone();
    std::fs::create_dir_all(&state).with_context(|| format!("create {}", state.display()))?;
    // The operator's kill switch: the mode file in the state directory wins over the environment,
    // and the entry reads it as it turns someone new away, so pausing the host takes no restart.
    let mode = tilo_workspace_service::workspaces::mode::HostMode::held(
        state.clone(),
        tilo_workspace_service::workspaces::mode::Mode::Normal,
    );
    // The deployment's own routes answer from the start: its health check says
    // the host is starting while it opens its workspaces, which can take a
    // minute, and that it serves once people can get in.
    let management = match persistent.http {
        Some(listen) => Some(http::listen(listen, Arc::clone(&mode)).await?),
        None => None,
    };
    let listen = args.web.web.context("a workspace needs --web")?;
    let mut operator = operator_lines()?;
    let store = tilo_secrets::SecretStore::for_app(SECRET_APP);
    let sign_in = discord_sign_in::SignIn::from_args(&persistent)?;
    let moderation = persistent
        .moderation_key_file
        .as_deref()
        .map(|key_file| {
            crate::persistent_moderation::load(
                &store,
                key_file,
                persistent.moderation_model.as_deref(),
            )
        })
        .transpose()?;
    if let Some(agent) = &moderation {
        tracing::info!(model = agent.model(), "moderation agent loaded");
    }
    // The same model looks over the reports the workspace's members write
    // before they send them, asked through the workspace on their behalf.
    let feedback_reviewer = moderation
        .is_some()
        .then(|| {
            crate::persistent_moderation::WorkspaceReviewer::from_store(
                &store,
                persistent.moderation_model.as_deref(),
            )
        })
        .transpose()?
        .map(|reviewer| {
            std::sync::Arc::new(reviewer)
                as std::sync::Arc<dyn tilo_agent_session::FeedbackReviewer>
        });
    let host = host_key(&state)?;
    let server_cert = enrol(
        &root,
        &host,
        &format!("{name} workspace host"),
        tilo_user_identity::DeviceCapabilities::member(),
    )?;
    // A state an earlier build kept under the folder's earlier name is not used:
    // the server says where it lies instead of coming up empty beside it without a word.
    if let Some(earlier) = tilo_server_protocol::metrics::earlier_server_root_beside(&state) {
        let own = tilo_server_protocol::metrics::workspace_server_root(&state);
        tracing::warn!(
            own = %own.display(),
            earlier = %earlier.display(),
            "an earlier build's state lies beside this server's empty folder; none of it is used"
        );
    }
    let config = tilo_server::ServerConfig {
        state_root: tilo_server_protocol::metrics::workspace_server_root(&state),
        identity_root: state.join("identity"),
        iroh_listen: persistent
            .iroh_listen
            .unwrap_or_else(|| tilo_server::ServerConfig::default().iroh_listen),
        work: hold_for(persistent.hold_for.as_deref())?,
        ..Default::default()
    };
    let identity = hosted_identity(&root, host, server_cert, &config.state_root)?;
    // A host that places its workspaces' browser sessions certifies them as
    // the person's stand-in, which it makes from the root it holds: nobody
    // else approves for them here.
    if tilo_server::hosted_browser::HostedBrowsers::from_env()
        .map_err(anyhow::Error::msg)?
        .is_some()
    {
        tilo_server::hosted_browser::keep_stand_in(&config.state_root, &root)
            .map_err(anyhow::Error::msg)?;
    }
    let hosted = start_server_lending(
        config,
        identity,
        &args.web,
        &state,
        crate::web_cli::Lent { feedback_reviewer },
    )
    .await?;
    // Everything a start settles and begins, in one place: a step that fails
    // stops the server it was started on before the error goes up (the `match`
    // below).
    let serving = async {
        let workspaces = hosted
            .server
            .workspaces()
            .context("the server governs no workspaces in this mode")?
            .clone();
        let shown = persistent
            .workspace_name
            .clone()
            .unwrap_or_else(|| workspace_name(&name));
        let host = args.web.public_origin.as_deref().map_or_else(
            || shown.clone(),
            |origin| {
                let host = origin.split_once("://").map_or(origin, |(_, host)| host);
                host.trim_end_matches('/').to_owned()
            },
        );
        workspaces
            .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer {
                host,
                storage_bytes: persistent.person_storage_gb.saturating_mul(1_000_000_000),
            })
            .map_err(|error| anyhow::anyhow!("read whose own workspaces are whose: {error}"))?;
        // A workspace this server is to be handed is handed through its site,
        // whose `/host` names this server to the holder handing it over: the
        // browser listener serves before the wait. One this server founds is
        // served only once its entry is settled below, since until then a fresh
        // workspace lets anyone in; a handed one comes with its own entry.
        let entry_wait_list = tilo_server::web::WaitListSlot::default();
        let kept = read_workspace_id(&id_file)?;
        let mut browsers = None;
        if let Some(kept) = &kept
            && !holds(&workspaces, kept).await
        {
            browsers = Some(
                serve_browsers(&hosted.server, &args, listen, &state, &entry_wait_list).await?,
            );
        }
        let workspace =
            governed_workspace(&workspaces, &shown, kept, &id_file, &store, &root).await?;
        // Its own workspaces are for the people this workspace's entry, or an
        // invitation into it, let in: never for anyone holding a root alone.
        workspaces.admit_own_through(&workspace);
        workspaces.call_private_model_addresses(persistent.call_own_network);
        if persistent.openai_sign_in.is_some()
            || persistent.chatgpt_backend.is_some()
            || persistent.openai_api.is_some()
        {
            let real = tilo_server::workspaces::OpenAiSignIn::default();
            workspaces.sign_in_to_openai_at(tilo_server::workspaces::OpenAiSignIn {
                issuer: persistent.openai_sign_in.clone().unwrap_or(real.issuer),
                backend: persistent.chatgpt_backend.clone().unwrap_or(real.backend),
                api: persistent.openai_api.clone().unwrap_or(real.api),
                client_id: real.client_id,
            });
        }
        // The certificate this host serves the workspace with: a build renamed
        // the host's own after a workspace was made with it, and the host took
        // the record's back (`Workspaces::serving_certificate`). Every governed
        // operation below names it, so a renamed certificate is no refusal.
        let server_cert = workspaces.serving_certificate().clone();
        // A setting of the operator's that the workspace's governance refuses
        // stops the start only where going on without it would let in more than
        // the operator configured: the entry, the guests' outside effects and the
        // removals above. The rest is said and settled again at the next start.
        settled(
            &workspace,
            settle_listing(&workspaces, &workspace, &server_cert, &persistent).await,
        );
        settle_invitation_only(&workspaces, &workspace, &server_cert, &persistent)
            .await
            .map_err(|error| anyhow::anyhow!("settle the workspace's entry: {error}"))?;
        // Its applications are set up by the one holder that governs them: on
        // a copy another server of the same person governs, that server set
        // them up, and this one does once it governs them, at a start then.
        if workspaces
            .governs_applications(&workspace)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?
        {
            seed_apps(&workspaces, &workspace, &server_cert, &persistent.seed_apps).await?;
            settle_chat(
                &workspaces,
                &workspace,
                &server_cert,
                &persistent.seed_apps,
                &persistent.chat_channels,
                persistent.feedback_channel.as_deref(),
                workspace_files(&persistent),
                &persistent.discussions,
            )
            .await?;
        } else {
            tracing::info!(
                %workspace,
                "another holder governs the workspace's applications: this server leaves their setup to it"
            );
        }
        settle_strips(&workspaces, &workspace, &server_cert, &persistent.strips).await?;
        ensure_guest_policy(&workspaces, &workspace, &server_cert).await?;
        settle_moderation(&workspaces, &workspace, &persistent).await?;
        // Members inviting, or not, never lets in anyone the entry does not.
        settled(
            &workspace,
            settle_member_invites(
                &workspaces,
                &workspace,
                &server_cert,
                persistent.members_only,
            )
            .await,
        );
        settle_owners(&workspaces, &workspace, &persistent.owners).await?;
        // A record the operator named whose retained history lost the base its
        // deltas were written over: founded again from what the workspace still
        // reads, its old history archived beside the replay. Runs once per start
        // that names it; a record that still reads is left as it is.
        refound_records(&workspaces, &workspace, &server_cert, &persistent.refound).await?;
        // The wait list, when the workspace takes people by invitation and
        // keeps one: the addresses' windows and the mail the entry sends from
        // its page (the web entry, or the entry `--wait-list-mail` names). What
        // the workspace's entry keeps is its governance's, settled above.
        let entry_policy = workspaces
            .entry_policy(&workspace)
            .await
            .unwrap_or_default();
        let settings = entry::WaitListSettings::from_env(
            entry_policy.wait_list_max,
            persistent
                .wait_list_mail
                .clone()
                .or_else(|| args.web.web_entry.clone()),
            std::env::var_os("TILO_WORKSPACES_LOCAL").is_some(),
        )
        .context("read the entry's mail settings")?;
        let wait_list =
            entry::WaitListPage::open(&workspaces, &workspace, &shown, &state, &settings)
                .context("open the entry's wait list")?;
        let page = wait_list.map(std::sync::Arc::new);
        if let Some(page) = &page {
            let _ = workspaces.offer_wait_list(page.offered());
            tracing::info!(
                workspace = %workspace,
                max = page.max(),
                mail = page.mails(),
                "the entry keeps a wait list"
            );
        }
        if let Some(page) = &page {
            let _ = entry_wait_list.set(Arc::clone(page));
        }
        let web = match browsers {
            Some(web) => web,
            None => serve_browsers(&hosted.server, &args, listen, &state, &entry_wait_list).await?,
        };
        println!(
            "{}",
            OperatorEvent::Started {
                version: operator::VERSION,
                origin: web.origin.clone(),
                plain_origin: web.plain_origin.clone(),
                user_public_key: hex::encode(root.verifying_key().to_bytes()),
                certificate_sha256: web.certificate_sha256.clone(),
                allowed_origins: args.web.allowed_origins.clone(),
                workspace: Some(workspace.clone()),
                host_info: Some(Box::new(web.host_info().clone())),
            }
            .to_line()
        );
        // The moderation agent runs as the workspace's bot for as long as the workspace
        // does, and looks at reports while a person who governs it has it turned on;
        // it answers reports left open while it was off or the workspace was down first.
        if moderation.is_some() {
            workspaces.offer_moderation(&workspace).await;
        }
        let moderator = moderation.map(|agent| {
            tokio::spawn(
                tilo_server::workspace_bots::ModerationBot::new(
                    workspaces.clone(),
                    workspace.clone(),
                    server_cert.clone(),
                    root.clone(),
                    agent,
                )
                .run(),
            )
        });
        // The connectors bots listen to, the operator's and those a member
        // sets up for a bot they ask for, share one host and keep what they
        // fetch in the workspace's content.
        let public = match (&persistent.public_avatars, &persistent.public_avatars_url) {
            (Some(dir), Some(url)) => Some(
                tilo_server::public_avatars::PublicAvatars::new(dir.clone(), url)
                    .with_context(|| format!("prepare {}", dir.display()))?,
            ),
            _ => None,
        };
        let connector_host = tilo_server::workspace_bots::installed::ConnectorHost::new(
            workers,
            // The server's content, where it keeps each workspace's files, uploads
            // and what its apps made: one store per workspace for everything this
            // host keeps.
            hosted.server.content(),
            public,
            tilo_server::workspace_bots::discord::Served {
                avatars_url: persistent.public_avatars_url.clone(),
                web_entry: args.web.web_entry.clone(),
                origin: Some(web.origin.clone()),
                discord_api_base: persistent.discord_api_base.clone(),
            },
        );
        let (mut runners, discord) = start_bots(
            &workspaces,
            &workspace,
            &server_cert,
            &root,
            &bots,
            &connector_host,
            &persistent,
            &state,
        )
        .await?;
        if let Some(discord) = &discord {
            offer_sign_in(discord, &persistent).await;
        }
        // The bridge carries the workspace's calls to Discord's voice channels
        // as the host's own device in the workspace's live session.
        runners.extend(discord.as_ref().and_then(|discord| {
            let relay = hosted.server.live_relay()?;
            Some(tokio::spawn(
                tilo_server::workspace_bots::discord_voice::VoiceBridge::new(
                    workspaces.clone(),
                    workspace.clone(),
                    server_cert.clone(),
                    relay,
                    Arc::clone(discord),
                )
                .run(),
            ))
        }));
        // The files members post in the workspace's Chat, kept in the same store
        // as what the bots fetch, so they stay once their poster left.
        let posted = connector_host
            .store(&workspace)
            .map_err(|error| anyhow::anyhow!("the workspace's content: {error}"))?;
        let attachments = attachment_limits(&persistent).map(|limits| {
            tokio::spawn(
                tilo_server::workspace_bots::attachments::WorkspaceAttachments::new(
                    workspaces.clone(),
                    workspace.clone(),
                    server_cert.device_public_key,
                    posted,
                    state.join("attachments.json"),
                    limits,
                )
                .run(),
            )
        });
        // Bots the workspaces' members ask for, beside the operator's: one
        // follower for every workspace this host governs, the Lounge and the
        // workspaces its people keep of their own, as each of them is made.
        let requested = tokio::spawn(
            tilo_server::workspace_bots::requested::Following {
                workspaces: workspaces.clone(),
                host: server_cert.clone(),
                root: root.clone(),
                state: state.clone(),
                operator: BTreeMap::from([(
                    workspace.clone(),
                    bots.iter().map(|spec| spec.entry.clone()).collect(),
                )]),
                connectors: Some(connector_host),
            }
            .run(),
        );
        let context = PersistentWorkspace {
            workspaces,
            workspace,
            web,
        };
        // The deployment's own routes serve once the workspace does, so a
        // health check that answers `up` means people can get in, and the
        // address they bound is announced then, so a deployment can reach a
        // port it let the kernel pick.
        if let Some(management) = &management {
            management.serve(http::Serving {
                avatars: persistent.public_avatars.clone(),
                sign_in,
                web_entry: args.web.web_entry.clone(),
                discord: discord.clone(),
            });
            println!(
                "{}",
                OperatorEvent::DeploymentRoutes {
                    origin: format!("http://{}", management.addr),
                }
                .to_line()
            );
        }
        // The operator's entry to the running workspace, for a deployment whose
        // workspace has no stdin: a socket the state volume carries.
        let owner_socket = match &persistent.operator_socket {
            Some(path) => {
                // A socket left by a workspace that stopped answers nobody.
                let _ = std::fs::remove_file(path);
                // The volume's path is longer than a socket's name may be on some
                // deployments, so it is bound through its directory.
                let bound = tilo_files::native::socket::bind(path)
                    .with_context(|| format!("bind {}", path.display()))?;
                bound
                    .set_nonblocking(true)
                    .with_context(|| format!("bind {}", path.display()))?;
                Some(
                    tokio::net::UnixListener::from_std(bound)
                        .with_context(|| format!("bind {}", path.display()))?,
                )
                .map(|socket| {
                    // A deployment's workspace runs as a user of its own, and
                    // its operator is the person who holds the volume: the
                    // socket is reachable exactly by whoever can reach the
                    // workspace's state, which those permissions already say.
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
                        .with_context(|| format!("let the operator reach {}", path.display()))?;
                    Ok::<_, anyhow::Error>(socket)
                })
                .transpose()?
            }
            None => None,
        };
        Ok::<Serving, anyhow::Error>(Serving {
            moderator,
            runners,
            attachments,
            requested,
            context,
            management,
            wait_list: page,
            owner_socket,
        })
    };
    // A start can wait for its workspace to be handed over for as long as no
    // holder hands it: a stop meanwhile ends the wait.
    let mut shutdown = std::pin::pin!(shutdown);
    let serving = tokio::select! {
        serving = serving => serving,
        result = &mut shutdown => {
            hosted.shutdown().await;
            return result;
        }
    };
    let Serving {
        moderator,
        runners,
        attachments,
        requested,
        context,
        management: _management,
        wait_list,
        owner_socket,
    } = match serving {
        Ok(serving) => serving,
        Err(error) => {
            // A start that failed stops the server it began before its error
            // goes up: a server dropped from here closes its endpoint after the
            // runtime is gone, with its tasks still joining what they started.
            hosted.shutdown().await;
            return Err(error);
        }
    };
    let mut invitations = wait_list
        .as_ref()
        .and_then(|page| page.invitations(&context.workspaces));
    // The workspace's entry admits under the same switch `/mode` reports, from the moment it
    // serves.
    context.workspaces.hold_mode(mode);
    let result = tokio::select! {
        result = serve_operator(&context, &mut operator, invitations.as_mut(), owner_socket.as_ref()) => result,
        result = &mut shutdown => result,
    };
    // Its agents' turns end first, while everything they work with is still up.
    hosted.finish_turns().await;
    for runner in moderator
        .into_iter()
        .chain(runners)
        .chain(attachments)
        .chain(std::iter::once(requested))
    {
        runner.abort();
    }
    drop(context);
    hosted.shutdown().await;
    result
}

/// What a started workspace keeps running until its operator stops it.
struct Serving {
    /// The moderation bot, when the operator gave it a model.
    moderator: Option<tokio::task::JoinHandle<()>>,
    /// The bots' tasks.
    runners: Vec<tokio::task::JoinHandle<()>>,
    /// The task posting the files members keep in the workspace's Chat.
    attachments: Option<tokio::task::JoinHandle<()>>,
    /// The task following the bots the governed workspaces' members ask for.
    requested: tokio::task::JoinHandle<()>,
    /// The workspace the operator's loop serves.
    context: PersistentWorkspace,
    /// The deployment's own routes, which stop when dropped.
    management: Option<http::Handle>,
    /// The wait list the entry keeps, when the workspace takes people by
    /// invitation and keeps one.
    wait_list: Option<std::sync::Arc<entry::WaitListPage>>,
    /// The socket the operator declares owners through, when it named one.
    owner_socket: Option<tokio::net::UnixListener>,
}

async fn summary(
    context: &PersistentWorkspace,
) -> anyhow::Result<tilo_server_session::workspaces::WorkspaceSummary> {
    context
        .workspaces
        .summary(&context.workspace)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// Addresses left at the entry and not confirmed in time are let go, also
/// while nobody touches the wait list.
async fn expire_wait_list(context: &PersistentWorkspace) {
    if context.entry_policy().await.wait_list_max == 0 {
        return;
    }
    if let Err(error) = context
        .workspaces
        .expire_wait_list(&context.workspace)
        .await
    {
        tracing::warn!("the wait list kept unconfirmed addresses: {error}");
    }
}

/// What the workspace last said about who is online.
struct OnlineWatch {
    last_online: Option<OperatorEvent>,
}

impl OnlineWatch {
    /// Say who is online when it changed. Who is here now is the workspace's
    /// presence; membership is the workspace's governance, and no one stops
    /// being a member by being away.
    async fn observe(&mut self, context: &PersistentWorkspace) -> anyhow::Result<()> {
        let sessions = context.web.online_users();
        let people = sessions.len();
        // Who of the members is here, for the site's directory.
        let here: std::collections::BTreeSet<[u8; 32]> = sessions.keys().copied().collect();
        let (faces, _, members) = context
            .workspaces
            .presence(&context.workspace, &here)
            .await
            .unwrap_or_default();
        let online = OperatorEvent::Online {
            people: u32::try_from(people).unwrap_or(u32::MAX),
            here: faces,
            members,
            online: here.iter().map(hex::encode).collect(),
        };
        if self.last_online.as_ref() != Some(&online) {
            println!("{}", online.to_line());
            self.last_online = Some(online);
        }
        Ok(())
    }
}

/// The operator's next connection to the workspace's owner socket, waiting
/// when the deployment named none.
async fn accept_operator(
    socket: Option<&tokio::net::UnixListener>,
) -> Option<tokio::net::UnixStream> {
    match socket {
        Some(socket) => socket.accept().await.ok().map(|(stream, _)| stream),
        None => std::future::pending().await,
    }
}

/// What a command on the operator socket works with: the workspace's
/// governance, its name and the host's own device. Copied for each connection,
/// which is served on a task of its own: a command that asks the entry to mail
/// an invitation waits on the loop that mails them, so serving it there would
/// wait on itself.
#[derive(Clone)]
struct OperatorContext {
    workspaces: Workspaces,
    workspace: String,
}

impl OperatorContext {
    fn of(context: &PersistentWorkspace) -> Self {
        Self {
            workspaces: context.workspaces.clone(),
            workspace: context.workspace.clone(),
        }
    }
}

/// Answer one operator line on the socket: the same command its stdin takes,
/// for a deployment whose workspace runs in a container and so has no stdin to
/// write to.
async fn answer_operator_socket(context: OperatorContext, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    let (reading, mut writing) = stream.into_split();
    let mut line = String::new();
    let mut reading = tokio::io::BufReader::new(reading);
    let answer = match reading.read_line(&mut line).await {
        Ok(0) => return,
        Ok(_) => match OperatorCommand::from_line(&line) {
            // A question about an address, and nothing about the address in
            // its answer: the operator may ask it of a running workspace.
            Ok(OperatorCommand::Status {
                request,
                removal_sha256,
            }) => answer_state(
                &context.workspaces,
                &context.workspace,
                request,
                &removal_sha256,
            )
            .await
            .to_line(),
            Ok(other) => OperatorEvent::Unanswered {
                reason: format!(
                    "the operator socket takes state, not {}; owners and administrators let \
                     people in with `tilo workspace people add` and `tilo workspace people invite`, and owners \
                     choose a workspace's servers with `tilo workspace servers`",
                    other.to_line().trim()
                ),
            }
            .to_line(),
            Err(error) => OperatorEvent::Unanswered {
                reason: error.to_string(),
            }
            .to_line(),
        },
        Err(error) => {
            tracing::warn!("the operator socket: {error}");
            return;
        }
    };
    if let Err(error) = writing.write_all(answer.as_bytes()).await {
        tracing::warn!("the operator socket's answer: {error}");
    }
    let _ = writing.shutdown().await;
}

async fn serve_operator(
    context: &PersistentWorkspace,
    operator: &mut tokio::sync::mpsc::Receiver<Result<OperatorCommand, String>>,
    mail: Option<&mut entry::InvitationMail>,
    owner_socket: Option<&tokio::net::UnixListener>,
) -> anyhow::Result<()> {
    let mut changes = context.workspaces.governance_changes();
    let removals = Removals::load(&summary(context).await?.members);
    let mut presence = OnlineWatch { last_online: None };
    let mut online_tick = tokio::time::interval(Duration::from_secs(operator::ONLINE_EVERY_SECS));
    let mut sweep_tick = tokio::time::interval(Duration::from_secs(WAIT_LIST_SWEEP_SECS));
    let mut mail = mail;
    // A deployment that runs the workspace without an operator leaves its input at
    // its end: the workspace then keeps serving, and only a command ends it.
    let mut operator_open = true;
    loop {
        tokio::select! {
            () = async {
                match mail.as_mut() {
                    Some(mail) => { mail.mail_next().await; }
                    None => std::future::pending().await,
                }
            } => {}
            _ = online_tick.tick() => {
                presence.observe(context).await?;
            }
            _ = sweep_tick.tick() => expire_wait_list(context).await,
            changed = changes.recv() => {
                match changed {
                    Ok(workspace) if workspace != context.workspace => continue,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => bail!("the server stopped governing"),
                }
                let members = summary(context).await?.members;
                for user in removals.taken_in(&members) {
                    // Bans exist once workspace moderation lands; until then every
                    // removal is a plain removal. The service treats both alike.
                    println!("{}", OperatorEvent::MemberRemoved { user_public_key: hex::encode(user), banned: false }.to_line());
                }
            }
            accepted = accept_operator(owner_socket) => {
                if let Some(stream) = accepted {
                    let operator = OperatorContext::of(context);
                    tokio::spawn(async move { answer_operator_socket(operator, stream).await });
                }
            }
            line = operator.recv(), if operator_open => match line {
                None => {
                    operator_open = false;
                    tracing::info!(
                        "the operator closed its input; the workspace keeps serving"
                    );
                }
                Some(Ok(command @ (OperatorCommand::WaitList { .. }
                | OperatorCommand::Unlist { .. }
                | OperatorCommand::Confirm { .. }
                | OperatorCommand::Status { .. }))) => {
                    println!("{}", answer_wait_list_command(context, command).await.to_line());
                }
                // An invitation the entry mailed itself is acknowledged here:
                // nothing else in the workspace hears of it.
                Some(Ok(OperatorCommand::InviteMailQueued { .. })) => {}
                Some(Ok(OperatorCommand::Admit { .. })) => bail!("a persistent workspace admits guests through its entry"),
                Some(Ok(OperatorCommand::Shutdown {})) => return Ok(()),
                Some(Err(error)) => bail!("unreadable workspace operator line: {error}"),
            },
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use tilo_server_protocol::workspaces::{JoinReply, WorkspacesReply, WorkspacesRequest};
    use tilo_user_identity::DeviceCapabilities;
    const NS_PER_SEC: u64 = 1_000_000_000;

    fn store() -> tilo_secrets::SecretStore {
        tilo_secrets::SecretStore::memory(SECRET_APP)
    }

    fn member(key: u8, administrator: bool) -> tilo_peer_state::Member {
        let mut row = tilo_peer_state::Member::of_person(
            &tilo_peer_state::join::Person::new(hex::encode([key; 32]), ""),
            Some(tilo_peer_state::join::Role::Editor),
        );
        row.administrator = administrator;
        row
    }

    tilo_test_env::scenario! {
        fn the_operators_arguments_name_the_workspace_its_quota_and_its_bots;

        phase the_workspace_is_named_after_its_directory_capitalized;
        {
            assert_eq!(workspace_name("plaza"), "Plaza");
            assert_eq!(workspace_name(""), "");
        }

        phase a_host_keeps_ten_gigabytes_per_person_unless_the_operator_says_otherwise;
        {
            use clap::Parser as _;
            #[derive(clap::Parser)]
            struct Args {
                #[command(flatten)]
                persistent: PersistentArgs,
            }
            let serving = |extra: &[&str]| {
                Args::try_parse_from(
                    [
                        "server",
                        "--persistent",
                        "tilo",
                        "--root-file",
                        "/run/secrets/root",
                        "--workspace-id-file",
                        "/state/id",
                    ]
                    .into_iter()
                    .chain(extra.iter().copied()),
                )
            };
            assert_eq!(
                serving(&[]).unwrap().persistent.person_storage_gb,
                10,
                "the host's default"
            );
            assert_eq!(
                serving(&["--person-storage-gb", "40"])
                    .unwrap()
                    .persistent
                    .person_storage_gb,
                40,
                "the operator sets it"
            );
            assert!(
                serving(&["--person-storage-gb", "lots"]).is_err(),
                "gigabytes are a number"
            );
            assert!(
                Args::try_parse_from(["server", "--person-storage-gb", "40"]).is_err(),
                "workspaces of people's own are kept by a persistent host"
            );
        }

        phase a_bot_spec_names_an_entry_and_its_inputs;
        {
            let spec = parse_bot_spec("so.tilo.bots.discord-bridge guild=123 mode=live").unwrap();
            assert_eq!(spec.entry, "so.tilo.bots.discord-bridge");
            assert_eq!(spec.inputs.get("guild").map(String::as_str), Some("123"));
            assert_eq!(input_value("123"), serde_json::json!("123"));
            assert_eq!(
                input_value(r#"{"link":"https://tilo.so"}"#),
                serde_json::json!({"link": "https://tilo.so"})
            );
            assert!(parse_bot_spec("so.tilo.bots.discord-bridge guild").is_err());
            assert!(parse_bot_spec("").is_err());
            assert!(
                parse_bot_spec("so.tilo.bots.x a=1 a=2")
                    .unwrap_err()
                    .to_string()
                    .contains("twice")
            );
        }
    }

    #[test]
    fn a_workspace_id_kept_in_its_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let id = "1ceb1ba6-9190-4b17-8208-5a71662ceb84";
        let named = dir.path().join("workspace-id");
        assert_eq!(read_workspace_id(&named).unwrap(), None);
        std::fs::write(&named, format!("{id}\n")).unwrap();
        assert_eq!(read_workspace_id(&named).unwrap().as_deref(), Some(id));
    }

    #[test]
    fn a_root_file_is_sixty_four_hex_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root");
        std::fs::write(&path, format!("{}\n", "11".repeat(32))).unwrap();
        assert_eq!(read_root_file(&path).unwrap().to_bytes(), [0x11; 32]);
        std::fs::write(&path, "not a key").unwrap();
        assert!(read_root_file(&path).is_err());
        assert!(
            read_root_file(&dir.path().join("missing"))
                .unwrap_err()
                .to_string()
                .contains("refuses to start")
        );
    }

    #[test]
    fn a_mounted_root_must_match_the_workspaces_stored_owner() {
        let store = store();
        let root = SigningKey::from_bytes(&[0x11; 32]);
        let other = SigningKey::from_bytes(&[0x22; 32]);
        check_stored_root(&store, "workspace-a", &root).expect_err("no stored root is refused");
        keep_root(&store, "workspace-a", &root).unwrap();
        check_stored_root(&store, "workspace-a", &root).unwrap();
        let refused = check_stored_root(&store, "workspace-a", &other).unwrap_err();
        assert!(
            refused.to_string().contains("not this workspace's owner"),
            "{refused:#}"
        );
    }

    #[test]
    fn removals_are_reported_once_and_only_for_newly_removed_people() {
        let removed = |key: u8, removed: bool| tilo_peer_state::Member {
            removed,
            ..member(key, false)
        };
        let mut seen = BTreeSet::from([[1; 32]]);
        let newly = newly_removed(
            &mut seen,
            &[removed(1, true), removed(2, true), removed(3, false)],
        );
        assert_eq!(newly, vec![[2; 32]]);
        assert!(newly_removed(&mut seen, &[removed(2, true)]).is_empty());
    }

    #[test]
    fn someone_admitted_again_is_reported_when_removed_again() {
        let listed = |removed: bool| tilo_peer_state::Member {
            removed,
            ..member(4, false)
        };
        let mut seen = BTreeSet::new();
        assert_eq!(newly_removed(&mut seen, &[listed(true)]), vec![[4; 32]]);
        assert!(newly_removed(&mut seen, &[listed(false)]).is_empty());
        assert_eq!(newly_removed(&mut seen, &[listed(true)]), vec![[4; 32]]);
    }

    /// A server governing one workspace `Plaza`, its workspaces and the host's
    /// device certificate.
    async fn plaza(dir: &Path) -> (tilo_server::daemon::Server, Workspaces, DeviceCert, String) {
        let (server, workspaces, server_cert) = host(dir).await;
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        (server, workspaces, server_cert, workspace)
    }

    /// Seeds as the operator's start does, naming the apps it asked `--seed-app`
    /// for, and answers with what the workspace seeded.
    async fn seeded(
        server_cert: &DeviceCert,
        workspaces: &Workspaces,
        workspace: &str,
        apps: &[&str],
    ) -> BTreeSet<String> {
        seed_apps(
            workspaces,
            workspace,
            server_cert,
            &apps.iter().map(|app| (*app).to_owned()).collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        settled_in(workspaces, workspace, "seeded").await.unwrap()
    }

    /// A persistent workspace's first start seeds its apps: a workspace that was
    /// just created holds no channel yet, so each seed's genesis waits for the
    /// group the host's channel service creates for it, and a wait that is too
    /// short stops the start.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fresh_workspace_seeds_its_apps_and_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let (server, workspaces, server_cert) =
            host_with(dir.path(), tilo_user_identity::DeviceCapabilities::member()).await;
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        let apps = ["so.tilo.tasks", "so.tilo.chat"];
        let first = seeded(&server_cert, &workspaces, &workspace, &apps).await;
        assert_eq!(
            first,
            apps.iter()
                .map(|app| (*app).to_owned())
                .collect::<BTreeSet<_>>()
        );
        // A restart seeds nothing again: what the first start seeded is kept.
        let again = seeded(&server_cert, &workspaces, &workspace, &apps).await;
        assert_eq!(again, first);
        server.shutdown().await;
    }

    /// The server of [`plaza`] started on `dir`, as every start signs its
    /// certificate afresh.
    async fn host(dir: &Path) -> (tilo_server::daemon::Server, Workspaces, DeviceCert) {
        host_with(dir, DeviceCapabilities::member()).await
    }

    /// [`host`] whose certificate carries `capabilities`: the host of a
    /// persistent workspace owns its workspaces, so it takes part in their
    /// channels too.
    async fn host_with(
        dir: &Path,
        capabilities: DeviceCapabilities,
    ) -> (tilo_server::daemon::Server, Workspaces, DeviceCert) {
        let root = SigningKey::from_bytes(&[1; 32]);
        // The host's device key outlives its starts, as `host-device.key` does.
        let kept = dir.join("host-device.key");
        let key: [u8; 32] = std::fs::read(&kept).map_or_else(
            |_| {
                let key = tilo_test_env::node_key();
                std::fs::write(&kept, key).unwrap();
                key
            },
            |bytes| bytes.try_into().unwrap(),
        );
        let host = SigningKey::from_bytes(&key);
        let server_cert = enrol(&root, &host, "host", capabilities).unwrap();
        let server = tilo_server::daemon::Server::start(
            tilo_server::ServerConfig {
                state_root: dir.join("server"),
                identity_root: dir.join("identity"),
                ..Default::default()
            }
            .on_loopback(),
            ServerIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host,
                device_cert: server_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let workspaces = server.workspaces().unwrap().clone();
        (server, workspaces, server_cert)
    }

    /// A host started as `tilo-server workspace` starts one: the operator's root
    /// file, the host device it keeps in its state, and the revocation log
    /// under that state.
    async fn start_workspace(
        root: &SigningKey,
        host: &SigningKey,
        server_cert: &DeviceCert,
        server_state: &Path,
        state: &Path,
    ) -> tilo_server::daemon::Server {
        let identity =
            hosted_identity(root, host.clone(), server_cert.clone(), server_state).unwrap();
        tilo_server::daemon::Server::start(
            tilo_server::ServerConfig {
                state_root: server_state.to_path_buf(),
                identity_root: state.join("identity"),
                ..Default::default()
            }
            .on_loopback(),
            identity,
        )
        .await
        .unwrap()
    }

    /// A workspace host reads its state's revocation log when it starts: a
    /// device the operator's root withdrew is refused as it connects, while
    /// another device of the same root still gets in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each host serves until the test is done with it"
    )]
    async fn a_revoked_device_is_refused_at_connect_and_its_sibling_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let server_state = tilo_server_protocol::metrics::workspace_server_root(&state);
        let root = SigningKey::from_bytes(&[0x11; 32]);
        let host = SigningKey::from_bytes(&[0x22; 32]);
        let server_cert = enrol(&root, &host, "host", DeviceCapabilities::member()).unwrap();
        let leaked = SigningKey::from_bytes(&[0x33; 32]);
        let leaked_cert = enrol(&root, &leaked, "leaked", DeviceCapabilities::member()).unwrap();
        let sibling = SigningKey::from_bytes(&[0x44; 32]);
        let sibling_cert = enrol(&root, &sibling, "sibling", DeviceCapabilities::member()).unwrap();
        let user = root.verifying_key().to_bytes();
        let endpoint = || async {
            iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .bind()
                .await
                .unwrap()
        };

        // Before the operator withdraws it, the device connects: what the
        // refusal below is the log's doing, not the device's.
        let server = start_workspace(&root, &host, &server_cert, &server_state, &state).await;
        let addr = server.node_addr();
        let admitted = tilo_server::ServerClient::connect(
            endpoint().await,
            addr,
            user,
            leaked_cert.clone(),
            leaked.clone(),
            RevocationList::new(),
        )
        .await;
        assert!(admitted.is_ok(), "an unrecognised device connects");
        server.shutdown().await;

        // The operator's revocation, written where the host reads it.
        let (_, changed) = tilo_server::root_signer::revoke_device(
            &server_state,
            &root,
            leaked_cert.device_public_key,
            tilo_user_identity::RevocationReason::Compromised,
        )
        .unwrap();
        assert!(changed, "a first revocation changes the log");

        let server = start_workspace(&root, &host, &server_cert, &server_state, &state).await;
        let addr = server.node_addr();
        let refused = tilo_server::ServerClient::connect(
            endpoint().await,
            addr.clone(),
            user,
            leaked_cert,
            leaked,
            RevocationList::new(),
        )
        .await;
        assert!(
            refused.is_err(),
            "the restarted host refuses the device its root revoked"
        );
        let admitted = tilo_server::ServerClient::connect(
            endpoint().await,
            addr,
            user,
            sibling_cert,
            sibling,
            RevocationList::new(),
        )
        .await;
        assert!(
            admitted.is_ok(),
            "another device of the same root still connects"
        );
    }

    /// Ask for an invitation to `workspace` as `device`, as a member's page does.
    async fn invite(
        workspaces: &Workspaces,
        workspace: &str,
        device: &DeviceCert,
    ) -> WorkspacesReply {
        workspaces
            .page_workspaces(
                device,
                WorkspacesRequest::Invite {
                    workspace: workspace.to_owned(),
                    request_id: rand::random(),
                    role: Role::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3600 * NS_PER_SEC),
                    inviter_name: Some("Member".into()),
                    wait_list_entry: None,
                    recipient: None,
                    administrator: false,
                },
            )
            .await
    }

    /// A fresh person with one device: their signing key and its certificate.
    fn guest(seed: u8) -> (SigningKey, DeviceCert) {
        let device = SigningKey::from_bytes(&[seed; 32]);
        let root = SigningKey::from_bytes(&[seed.wrapping_add(100); 32]);
        let cert = enrol(&root, &device, "guest", DeviceCapabilities::member()).unwrap();
        (device, cert)
    }

    /// Whether `person`'s device redeems the entry's `invitation` and `token`,
    /// as their page does.
    async fn walk_in(
        workspaces: &Workspaces,
        (_, cert): &(SigningKey, DeviceCert),
        invitation: Vec<u8>,
        token: &str,
    ) -> bool {
        matches!(
            workspaces
                .admit_proven(
                    invitation,
                    cert.clone(),
                    rand::random(),
                    "Guest",
                    Some(token.as_bytes()),
                )
                .await,
            JoinReply::Admitted { .. }
        )
    }

    /// A workspace the operator lists publicly gets its moderation bot's grant
    /// at start, and a restart changes nothing; one it does not list is left
    /// alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_public_workspace_is_granted_its_moderation_bot_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let (server, workspaces, serving, workspace) = plaza(dir.path()).await;
        let listed = PersistentArgs {
            listing_description: Some("A plaza".into()),
            ..PersistentArgs::default()
        };
        settle_moderation(&workspaces, &workspace, &listed)
            .await
            .unwrap();
        assert!(
            tilo_server::workspace_bots::enabled(&workspaces, &workspace)
                .await
                .unwrap(),
            "the Lounge the operator lists has its moderation bot"
        );
        let generation = workspaces.summary(&workspace).await.unwrap().generation;
        settle_moderation(&workspaces, &workspace, &listed)
            .await
            .unwrap();
        assert_eq!(
            workspaces.summary(&workspace).await.unwrap().generation,
            generation,
            "a restart changes nothing"
        );
        let quiet = workspaces.create("Quiet").await.unwrap().workspace;
        settle_moderation(&workspaces, &quiet, &PersistentArgs::default())
            .await
            .unwrap();
        assert!(
            !tilo_server::workspace_bots::enabled(&workspaces, &quiet)
                .await
                .unwrap(),
            "an unlisted workspace is not moderated"
        );
        // A person who governs turns it off; the next start leaves it off,
        // because the operator's setup had its one say already.
        workspaces
            .remove_workspace_grants(
                &workspace,
                &serving,
                [8; 32],
                vec![tilo_action_policy::GrantId::new(
                    tilo_action_policy::bot::MODERATION_GRANT,
                )],
            )
            .await
            .expect("the person takes the grant back");
        assert!(
            !tilo_server::workspace_bots::enabled(&workspaces, &workspace)
                .await
                .unwrap()
        );
        let off = workspaces.summary(&workspace).await.unwrap().generation;
        settle_moderation(&workspaces, &workspace, &listed)
            .await
            .unwrap();
        assert!(
            !tilo_server::workspace_bots::enabled(&workspaces, &workspace)
                .await
                .unwrap(),
            "a restart does not turn the person's off back on"
        );
        assert_eq!(
            workspaces.summary(&workspace).await.unwrap().generation,
            off,
            "and writes nothing"
        );
        server.shutdown().await;
    }

    /// An owner the operator declares gets in through an entry that takes
    /// people by invitation, and governs the workspace: the declaration is the
    /// workspace's own governance, so it is there before their device arrives
    /// and stays after a restart.
    #[tokio::test]
    async fn a_declared_owner_is_let_in_and_governs_from_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let (server, workspaces, server_cert, workspace) = plaza(dir.path()).await;
        ensure_guest_policy(&workspaces, &workspace, &server_cert)
            .await
            .unwrap();
        settle_member_invites(&workspaces, &workspace, &server_cert, false)
            .await
            .unwrap();
        workspaces
            .publish(
                &workspace,
                &server_cert,
                Listing {
                    description: "A plaza".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        settle_invitation_only(
            &workspaces,
            &workspace,
            &server_cert,
            &PersistentArgs {
                invitation_only: true,
                ..PersistentArgs::default()
            },
        )
        .await
        .unwrap();

        // Nobody is in the workspace yet, so this is how its first person
        // gets in: the entry takes people by invitation, and the operator
        // declares them.
        let owner = guest(31);
        let owners = [hex::encode(owner.1.user_public_key)];
        settle_owners(&workspaces, &workspace, &owners)
            .await
            .unwrap();
        // Named again at the next start, they are an owner already.
        settle_owners(&workspaces, &workspace, &owners)
            .await
            .unwrap();
        assert!(
            workspaces
                .is_owner(&workspace, owner.1.user_public_key)
                .await
        );
        // A stranger is turned away: the entry takes people by invitation, and
        // the declaration is what the workspace holds of the person it named.
        let (_, stranger) = guest(32);
        assert!(
            workspaces
                .admit_listed(
                    &workspace,
                    &tilo_server_protocol::directory::EntryJoin {
                        user_public_key: hex::encode(stranger.user_public_key),
                        display_name: "Eve".into(),
                    },
                )
                .await
                .is_err(),
            "a stranger holds no invitation and is in no declaration"
        );
        // In they walk, through the invitation-only entry.
        let (issued, token) = workspaces
            .admit_listed(
                &workspace,
                &tilo_server_protocol::directory::EntryJoin {
                    user_public_key: hex::encode(owner.1.user_public_key),
                    display_name: "Ada".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            walk_in(&workspaces, &owner, issued.signed_invitation, &token).await,
            "the entry lets a declared person in"
        );
        let members = workspaces.summary(&workspace).await.unwrap().members;
        assert!(
            members
                .iter()
                .any(|member| member.root() == Some(owner.1.user_public_key)
                    && member.administrator
                    && !member.removed),
            "the declared owner governs the workspace once their device is in"
        );
        // What is no key stops the start.
        let refused = settle_owners(&workspaces, &workspace, &["Ada".to_owned()]).await;
        assert!(refused.is_err(), "{refused:?}");
        server.shutdown().await;
    }

    /// Someone the listed workspace's entry lets in is a member; once the
    /// workspace takes people by invitation, a member invites someone with an
    /// invitation of their own, which admits them by itself, while the
    /// workspace's link is no invitation at all. Once it lets anyone in again,
    /// members no longer invite.
    #[tokio::test]
    async fn taking_people_by_invitation_the_invitation_admits_the_guest() {
        let dir = tempfile::tempdir().unwrap();
        let (server, workspaces, server_cert, workspace) = plaza(dir.path()).await;
        ensure_guest_policy(&workspaces, &workspace, &server_cert)
            .await
            .unwrap();
        // An open workspace settles as one at every start, holding no such grant.
        settle_member_invites(&workspaces, &workspace, &server_cert, false)
            .await
            .unwrap();

        // Someone who joined through the entry while the workspace was listed
        // is a member. A listed workspace's link is its invitation: members
        // invite nobody themselves.
        workspaces
            .publish(
                &workspace,
                &server_cert,
                Listing {
                    description: "A plaza".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let member = guest(12);
        let (issued, token) = workspaces
            .admit_listed(
                &workspace,
                &tilo_server_protocol::directory::EntryJoin {
                    user_public_key: hex::encode(member.1.user_public_key),
                    display_name: "Ada".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            walk_in(&workspaces, &member, issued.signed_invitation, &token).await,
            "the entry lets the first person in"
        );
        let asked = invite(&workspaces, &workspace, &member.1).await;
        assert!(matches!(asked, WorkspacesReply::Refused(_)), "{asked:?}");

        workspaces
            .unpublish(&workspace, &server_cert)
            .await
            .unwrap();
        settle_member_invites(&workspaces, &workspace, &server_cert, true)
            .await
            .unwrap();

        // The member invites the stranger, who redeems it: the invitation's
        // own grant admits them.
        let stranger = guest(11);
        let issued = invite(&workspaces, &workspace, &member.1).await;
        let WorkspacesReply::Invitation {
            signed_invitation, ..
        } = issued
        else {
            panic!("a member invites: {issued:?}");
        };
        let admitted = workspaces
            .admit_proven(
                signed_invitation,
                stranger.1.clone(),
                rand::random(),
                "Stranger",
                None,
            )
            .await;
        assert!(
            matches!(admitted, JoinReply::Admitted { .. }),
            "{admitted:?}"
        );
        let members = workspaces.summary(&workspace).await.unwrap().members;
        assert!(
            members
                .iter()
                .any(|member| member.root() == Some(stranger.1.user_public_key) && !member.removed),
            "the invited stranger is a member"
        );

        settle_member_invites(&workspaces, &workspace, &server_cert, false)
            .await
            .unwrap();
        let asked = invite(&workspaces, &workspace, &member.1).await;
        assert!(matches!(asked, WorkspacesReply::Refused(_)), "{asked:?}");
        server.shutdown().await;
    }

    /// A workspace the deployment handed to its owner is still its host's to
    /// settle: after a restart the host, the administrator it stays, writes
    /// the guests' Deny, members inviting (taken back and given again) and
    /// the moderation bot's grant as every start does, while the workspace
    /// stays its owner's, whom a deployment naming someone else does not
    /// replace.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_handed_workspace_takes_its_hosts_settings_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (server, workspaces, server_cert, workspace) = plaza(dir.path()).await;
        let listed = PersistentArgs {
            listing_description: Some("A plaza".into()),
            ..PersistentArgs::default()
        };
        ensure_guest_policy(&workspaces, &workspace, &server_cert)
            .await
            .unwrap();
        settle_moderation(&workspaces, &workspace, &listed)
            .await
            .unwrap();
        settle_member_invites(&workspaces, &workspace, &server_cert, false)
            .await
            .unwrap();
        let owner = guest(41).1.user_public_key;
        let owners = [hex::encode(owner)];
        settle_owners(&workspaces, &workspace, &owners)
            .await
            .unwrap();
        assert!(workspaces.is_owner(&workspace, owner).await);
        server.shutdown().await;

        let (server, workspaces, server_cert) = host(dir.path()).await;
        assert!(
            workspaces.governs_workspace(&workspace).await,
            "the host administers the workspace it handed over"
        );
        for step in 0..2 {
            ensure_guest_policy(&workspaces, &workspace, &server_cert)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_moderation(&workspaces, &workspace, &listed)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_member_invites(&workspaces, &workspace, &server_cert, false)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_member_invites(&workspaces, &workspace, &server_cert, true)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_owners(
                &workspaces,
                &workspace,
                &[hex::encode(guest(42).1.user_public_key)],
            )
            .await
            .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
        }
        let grants = workspaces.workspace_grants(&workspace).await.unwrap();
        for id in ["public-workspace-guests-no-outside", MEMBERS_INVITE] {
            assert!(
                grants.iter().any(|grant| grant.id.as_str() == id),
                "{id} is held: {grants:#?}"
            );
        }
        assert!(
            tilo_server::workspace_bots::enabled(&workspaces, &workspace)
                .await
                .unwrap()
        );
        assert!(
            workspaces.is_owner(&workspace, owner).await,
            "the workspace stays its owner's"
        );
        server.shutdown().await;
    }

    /// A server a workspace was handed to holds it and its own person
    /// administers it, while the device the workspace was made on stays the
    /// one its governance names to make its base: the grant is part of that
    /// holder's setup, so the copy enables no moderation bot and notes no
    /// grant as settled, and its start is not refused for a write it may not
    /// make.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_holder_that_sets_up_no_applications_leaves_the_moderation_grant_alone() {
        let made_on = tempfile::tempdir().unwrap();
        let handed_to = tempfile::tempdir().unwrap();
        let (server, workspaces, _server_cert, workspace) = plaza(made_on.path()).await;
        let (copy, copied, copy_cert) = host(handed_to.path()).await;
        workspaces
            .attach_server(
                &workspace,
                copy_cert.device_public_key,
                copy.node_addr(),
                tilo_peer_state::CopyRole::KeyHolder,
            )
            .await
            .unwrap();
        tilo_test_env::within("the handed copy holds the workspace", async {
            while !copied.governs_workspace(&workspace).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            copied.governs_workspace(&workspace).await,
            "the copy's person administers the workspace they were handed"
        );
        assert!(
            workspaces.governs_applications(&workspace).await.unwrap(),
            "the device the workspace was made on sets up its applications"
        );
        assert!(
            !copied.governs_applications(&workspace).await.unwrap(),
            "the copy's person administers it, but another holder sets up its applications"
        );
        let listed = PersistentArgs {
            listing_description: Some("A plaza".into()),
            ..PersistentArgs::default()
        };
        settle_moderation(&copied, &workspace, &listed)
            .await
            .expect("the copy leaves the grant to the holder that sets the applications up");
        assert!(
            !tilo_server::workspace_bots::enabled(&copied, &workspace)
                .await
                .unwrap(),
            "the copy enables no moderation bot of its own"
        );
        assert!(
            !settled_in(&copied, &workspace, "moderation")
                .await
                .unwrap()
                .contains(tilo_action_policy::bot::MODERATION_GRANT),
            "and notes no grant as settled"
        );
        copy.shutdown().await;
        server.shutdown().await;
    }

    /// A listed workspace people walked in through, with an owner declared,
    /// a moderator and someone removed, takes its host's start-time settings
    /// again after the host restarts, as the public workspace does at every start.
    #[tokio::test]
    async fn a_lived_in_workspace_takes_its_hosts_settings_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (server, workspaces, server_cert, workspace) = plaza(dir.path()).await;
        ensure_guest_policy(&workspaces, &workspace, &server_cert)
            .await
            .unwrap();
        settle_member_invites(&workspaces, &workspace, &server_cert, false)
            .await
            .unwrap();
        workspaces
            .publish(
                &workspace,
                &server_cert,
                Listing {
                    description: "A plaza".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let people = [guest(21), guest(22), guest(23)];
        for person in &people {
            let (issued, token) = workspaces
                .admit_listed(
                    &workspace,
                    &tilo_server_protocol::directory::EntryJoin {
                        user_public_key: hex::encode(person.1.user_public_key),
                        display_name: "Guest".into(),
                    },
                )
                .await
                .unwrap();
            assert!(walk_in(&workspaces, person, issued.signed_invitation, &token).await);
        }
        workspaces
            .set_member_administrator(
                &workspace,
                &server_cert,
                [1; 32],
                people[0].1.user_public_key,
                None,
            )
            .await
            .unwrap();
        workspaces
            .set_member_role(
                &workspace,
                &server_cert,
                tilo_server_session::workspaces::SetMemberRoleRequest {
                    request_id: [2; 32],
                    user_public_key: people[1].1.user_public_key,
                    role: tilo_peer_state::join::Role::Moderator,
                },
            )
            .await
            .unwrap();
        workspaces
            .ban_member(
                &workspace,
                &server_cert,
                tilo_server_session::workspaces::RemoveMemberRequest {
                    request_id: [3; 32],
                    user_public_key: people[2].1.user_public_key,
                },
            )
            .await
            .unwrap();
        server.shutdown().await;

        let (server, workspaces, server_cert) = host(dir.path()).await;
        for step in 0..2 {
            ensure_guest_policy(&workspaces, &workspace, &server_cert)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_member_invites(&workspaces, &workspace, &server_cert, false)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_member_invites(&workspaces, &workspace, &server_cert, true)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
        }
        let log = workspaces.membership_log(&workspace).await.unwrap();
        assert!(
            !log.iter().any(|line| line.contains("log against rows")),
            "{log:#?}"
        );
        server.shutdown().await;
    }
}
