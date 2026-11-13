//! `tilo-relay workspace --persistent`: one always-on public workspace. The workspace's
//! owner root comes from the operator's encrypted secrets (mounted as a
//! file) and is kept in this host's secret store, never in a record, the
//! environment or a log. This is the documented exception to "the host
//! never holds a workspace's root": nobody is present to approve guests, and
//! the workspace is public anyway.
//!
//! Anyone who joins is their own person, with the role the workspace's
//! listing gives them, through the same door as any public workspace
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
use tilo_relay::daemon::RelayIdentity;
use tilo_relay::web::WebHandle;
use tilo_relay::workspaces::{WaitListed, Workspaces};
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::operator::{self, OperatorCommand, OperatorEvent, hex32};
use tilo_relay_protocol::workspaces::Role;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::web_cli::{WorkspaceArgs, enrol, operator_lines, start_relay_lending, web_config};
use tilo_relay::door;

mod bots;
mod discord_sign_in;
mod http;
use bots::{offer_sign_in, start_bots};

/// The secret store this host keeps workspace roots in.
pub(crate) const SECRET_APP: &str = "so.tilo.relay";
/// How often unconfirmed addresses left at the door are let go.
const WAIT_LIST_SWEEP_SECS: u64 = 60;
/// `--author-app none`: the workspace authors nothing and is read-only.
const NO_AUTHORED_APPS: &str = "none";

#[derive(clap::Args, Clone, Debug, Default)]
pub struct PersistentArgs {
    /// Run one persistent public workspace with this name instead of a
    /// short-lived one.
    #[arg(long, value_name = "NAME", requires_all = ["root_file", "room_id_file"])]
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
        value_parser = crate::room_chat::channel_name
    )]
    pub chat_channels: Vec<String>,
    /// The channel, by name, the workspace's members report problems to: kept
    /// in the workspace's Chat on every start, after its other channels, since
    /// the site's pages send reports to it by its id (`c_<name>`).
    #[arg(
        long = "feedback-channel",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::room_chat::channel_name
    )]
    pub feedback_channel: Option<String>,
    /// A strip, by name, the workspace's shared desktop has in its first
    /// workspace; repeatable, in order. Added at start where missing; the
    /// strips people made are kept.
    #[arg(
        long = "strip",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::room_strips::strip_name
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
        value_parser = crate::room_discussions::parse
    )]
    pub discussions: Vec<crate::room_discussions::Published>,
    /// The workspace's owner root: a file of 64 hex characters (the Ed25519
    /// seed), mounted from the operator's encrypted secrets.
    #[arg(long = "root-file", value_name = "PATH")]
    pub root_file: Option<PathBuf>,
    /// Where the workspace's UUID is kept across restarts.
    #[arg(long = "room-id-file", value_name = "PATH")]
    pub room_id_file: Option<PathBuf>,
    /// Apply per-person change and content budgets to members.
    #[arg(long = "member-limits")]
    pub member_limits: bool,
    /// Where the host's own endpoint (iroh, UDP) listens: devices of the
    /// operator that dial it, such as the members that take its workspaces'
    /// work, name this port. Without it the machine picks one at each start.
    #[arg(long = "iroh-listen", value_name = "ADDRESS", requires = "persistent")]
    pub iroh_listen: Option<std::net::SocketAddr>,
    /// Every host keeps workspaces of people's own (`room.create-own`) for
    /// the people this workspace's door or an invitation into it let in, as
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
    /// anyone may join it through its door at `--default-role`. Asked once: the
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
    /// Take people by invitation: an unlisted workspace's door lets nobody new
    /// in, and members may invite someone with an invitation of their own,
    /// which they let in. Settled at every start. A listed workspace lets anyone
    /// in through its door, so this goes with no `--listing-description`.
    #[arg(long = "members-only", conflicts_with = "listing_description")]
    pub members_only: bool,
    /// List the workspace, its door admitting only people invited or
    /// pre-admitted: a member's invitation, or the one this door issued a
    /// person it let in. Anyone else is told the workspace is joined by
    /// invitation instead of being handed one. Asked once: the door is then
    /// the workspace's own setting. Goes with --listing-description, which
    /// --members-only cannot.
    #[arg(
        long = "invitation-only",
        requires = "listing_description",
        conflicts_with = "members_only"
    )]
    pub invitation_only: bool,
    /// How many requests left at the door the workspace keeps while it takes
    /// people by invitation: each is an address (an email) someone who asked
    /// without an invitation leaves, on the workspace's wait list for whoever
    /// may invite until they take it off. `0`: the door keeps none. It is the
    /// door taking people by invitation that turns them away, so it is that
    /// door this goes with, and the workspace holds it from then on.
    #[arg(
        long = "wait-list-max",
        value_name = "COUNT",
        default_value_t = 0,
        requires = "invitation_only"
    )]
    pub wait_list_max: usize,
    /// The web entry DOOR (a URL ending in `/`) the wait list's mails are
    /// sent from, which their links open: the confirmation link, and an
    /// invitation a member asks the door to mail to a confirmed address. It
    /// defaults to the workspace's own web entry; without mail configured,
    /// the list keeps addresses and mails nothing.
    #[arg(
        long = "wait-list-mail",
        value_name = "DOOR",
        requires = "wait_list_max"
    )]
    pub wait_list_mail: Option<String>,
    /// A UNIX socket the operator asks the running workspace through, one
    /// [`OperatorCommand::DeclareOwner`] line per connection: a deployment's
    /// workspace runs in a container, which has no stdin to write to. The
    /// answer is one [`OperatorEvent`] line on the same connection.
    #[arg(long = "operator-socket", value_name = "PATH")]
    pub operator_socket: Option<PathBuf>,
    /// An app authored into the workspace at its first start; repeatable.
    #[arg(long = "seed-app", value_name = "APP_ID")]
    pub seed_apps: Vec<String>,
    /// An app the workspace's members write through: it authors the workspace's
    /// content, so a member whose role lets them change the workspace (a guest
    /// is an Editor) can talk in its Chat; repeatable. Settled at every
    /// start: an app named here no longer stops authoring the workspace.
    /// Naming none authors every first-party app and every seeded one, since a
    /// workspace's app list is its policy and a public workspace offers its
    /// members every app; `none` authors none, and the workspace is read-only.
    #[arg(long = "author-app", value_name = "APP_ID")]
    pub author_apps: Vec<String>,
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
    /// room announces once it serves.
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
    #[arg(long = "attachment-room-mib", default_value_t = 2048)]
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
pub(crate) struct PersistentRoom {
    pub workspaces: Workspaces,
    pub workspace: String,
    pub host_cert: DeviceCert,
    pub web: WebHandle,
}

impl PersistentRoom {
    /// The workspace's door as its governance holds it: whether a directory
    /// lists it, whether it takes people by invitation, and how many requests
    /// it keeps.
    async fn door(&self) -> tilo_action_policy::DoorPolicy {
        self.workspaces
            .door_policy(&self.workspace)
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
        .item("room-root", workspace)
        .put_blocking(&root.to_bytes())
        .context("keep the workspace root in the host's secret store")
}

/// Keep the mounted root for a workspace that starts fresh under an id it
/// kept, checking it against the root already stored when there is one.
pub(crate) fn adopt_root(
    store: &tilo_secrets::SecretStore,
    workspace: &str,
    root: &SigningKey,
) -> anyhow::Result<()> {
    match store
        .item("room-root", workspace)
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
        .item("room-root", workspace)
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
    members: &[tilo_peer_state::GovernedMember],
) -> Vec<[u8; 32]> {
    // Someone admitted again since is a member: a later removal is new.
    for member in members.iter().filter(|member| !member.removed) {
        seen.remove(&member.user_public_key);
    }
    members
        .iter()
        .filter(|member| member.removed && seen.insert(member.user_public_key))
        .map(|member| member.user_public_key)
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
/// never handed out at the door.
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
/// `tilo-enroll revoke` writes that log, so a device the root withdrew is
/// refused when it connects, as on the plain daemon.
fn hosted_identity(
    root: &SigningKey,
    host: SigningKey,
    host_cert: DeviceCert,
    relay_state: &Path,
) -> anyhow::Result<RelayIdentity> {
    tilo_relay::daemon::with_persisted_revocations(
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host,
            device_cert: host_cert,
            revocations: RevocationList::new(),
        },
        relay_state,
    )
}

/// The host's own device key, kept beside the relay's state. The root is
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
            write_private(&path, &key.to_bytes()).context("save the host device key")?;
            Ok(key)
        }
        Err(error) => Err(error).context("read the host device key"),
    }
}

/// Write `bytes` to `path` atomically, readable by this user only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic(path, bytes, true)
}

fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    tilo_files::native::atomic::write_with_mode(path, bytes, if private { 0o600 } else { 0o644 })
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> anyhow::Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("read {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    write_atomic(path, &serde_json::to_vec(value)?, false)
        .with_context(|| format!("write {}", path.display()))
}

/// The governed workspace: the one named in `room_id_file`, else the only workspace
/// this relay governs (a start interrupted between keeping the root and
/// writing the file), else a new one. An id whose state is gone starts again
/// under it: the id is the workspace's address, which the id file keeps.
async fn governed_room(
    workspaces: &Workspaces,
    shown: &str,
    id_file: &Path,
    store: &tilo_secrets::SecretStore,
    root: &SigningKey,
) -> anyhow::Result<String> {
    let known = match std::fs::read_to_string(id_file) {
        Ok(text) => Some(text.trim().to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read the workspace id file"),
    };
    let workspace = match known {
        Some(workspace)
            if !workspaces
                .list()
                .await
                .iter()
                .any(|room| room.workspace == workspace) =>
        {
            let created = workspaces
                .create_workspace(shown, Some(&workspace))
                .await
                .map_err(anyhow::Error::msg)?;
            adopt_root(store, &created.workspace, root)?;
            created.workspace
        }
        Some(workspace) => workspace,
        None => match workspaces.list().await.as_slice() {
            [only] => only.workspace.clone(),
            [] => {
                // The root goes into the store before the workspace exists, so a
                // crash in between is re-adopted above, never orphaned.
                let created = workspaces.create(shown).await.map_err(anyhow::Error::msg)?;
                keep_root(store, &created.workspace, root)?;
                created.workspace
            }
            _ => bail!("this relay governs several rooms; a persistent workspace governs one"),
        },
    };
    check_stored_root(store, &workspace, root)?;
    workspaces
        .rename(&workspace, shown)
        .await
        .map_err(|error| anyhow::anyhow!("name the workspace {shown:?}: {error}"))?;
    write_atomic(id_file, workspace.as_bytes(), false).context("write the workspace id file")?;
    Ok(workspace)
}

/// Whom the workspace has taken in as removed, so each removal is reported
/// once. Whom the door keeps out is the workspace's own
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
    fn load(members: &[tilo_peer_state::GovernedMember]) -> Self {
        let seen = members
            .iter()
            .filter(|member| member.removed)
            .map(|member| member.user_public_key)
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
    fn taken_in(&self, members: &[tilo_peer_state::GovernedMember]) -> Vec<[u8; 32]> {
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

/// Keep the request someone left at the door without an invitation, while
/// the workspace takes people by invitation and keeps a wait list.
async fn answer_wait_list(context: &PersistentRoom, ask: WaitListAsk) -> OperatorEvent {
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
    let door = context.door().await;
    if !door.keeps_requests() {
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
    let secrets = tilo_relay::workspaces::WaitListSecrets { removal, confirm };
    match context
        .workspaces
        .wait(
            &context.workspace,
            &contact,
            secrets,
            &language,
            door.wait_list_max,
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
    context: &PersistentRoom,
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
    context: &PersistentRoom,
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
async fn answer_standing(
    context: &PersistentRoom,
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
        .wait_list_standing(&context.workspace, removal)
        .await
    {
        Ok(standing) => OperatorEvent::Standing { request, standing },
        Err(error) => OperatorEvent::WaitListFailed {
            request,
            reason: error.to_string(),
        },
    }
}

/// The answer to a command about the wait list.
async fn answer_wait_list_command(
    context: &PersistentRoom,
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
        OperatorCommand::Standing {
            request,
            removal_sha256,
        } => answer_standing(context, request, &removal_sha256).await,
        _ => OperatorEvent::WaitListFailed {
            request: String::new(),
            reason: "not a wait-list command".into(),
        },
    }
}

/// Declare the person `key` (their user root, hex) an owner of the workspace:
/// its governance holds them by their root as a member with every governance
/// action, so the door lets them in even when it takes people by invitation,
/// and a key of theirs lists the workspace, enters it and invites before any
/// device of theirs has joined. Answers what the operator hears.
async fn declare_owner(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    key: &str,
) -> String {
    let Some(user) = hex32(key) else {
        return OperatorEvent::OwnerRefused {
            user_public_key: key.to_owned(),
            reason: "a user root key is 64 lowercase hex characters".into(),
        }
        .to_line();
    };
    match workspaces
        .set_member_administrator(
            workspace,
            host,
            request_id(b"owner", &user, tilo_clock::unix_now_ns()),
            user,
        )
        .await
    {
        Ok(_) => OperatorEvent::OwnerDeclared {
            user_public_key: key.to_owned(),
        }
        .to_line(),
        Err(error) => OperatorEvent::OwnerRefused {
            user_public_key: key.to_owned(),
            reason: error.to_string(),
        }
        .to_line(),
    }
}

/// A fresh administration request id per decision: the same person can be
/// promoted, demoted and promoted again over the workspace's life.
fn request_id(kind: &[u8], key: &[u8; 32], now_ns: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind);
    hasher.update(key);
    hasher.update(&now_ns.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// Seed the workspace with `apps`: each one's record written from its own
/// factory, as the workspace's governor, and kept in `seeded_file` so a restart
/// seeds only what it added since.
///
/// A workspace the operator just created holds no channel yet, and its records
/// are sealed from their first write, so each seed's genesis waits for the
/// group the host's channel service creates for it: the wait is bounded by the
/// service (`crate::channels::OPEN_WAIT`, longer than the group may take to be
/// founded), and each app is named in the error when its keys never come.
pub(crate) async fn seed_apps(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    apps: &[String],
    seeded_file: &Path,
) -> anyhow::Result<()> {
    let mut seeded: BTreeSet<String> = read_json(seeded_file)?;
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let pending: Vec<&String> = apps.iter().filter(|app| !seeded.contains(*app)).collect();
    for app in pending {
        let record = tilo_relay::room_bots::workspace_record(&catalogue, app);
        workspaces
            .prepare_application(workspace, cert, &catalogue, app, &record)
            .await
            .map_err(|error| anyhow::anyhow!("seed {app}: {error}"))?;
        seeded.insert(app.clone());
        write_json(seeded_file, &seeded)?;
    }
    Ok(())
}

/// What the workspace keeps of the files members post in its Chat; `None`
/// when it keeps none.
fn attachment_limits(
    persistent: &PersistentArgs,
) -> Option<tilo_relay::room_bots::attachments::AttachmentLimits> {
    const MIB: u64 = 1024 * 1024;
    (persistent.attachment_room_mib > 0).then(|| {
        tilo_relay::room_bots::attachments::AttachmentLimits {
            file_bytes: persistent.attachment_max_mib.saturating_mul(MIB),
            member_bytes: persistent.attachment_member_mib.saturating_mul(MIB),
            workspace_bytes: persistent.attachment_room_mib.saturating_mul(MIB),
        }
    })
}

/// [`attachment_limits`] as the workspace's Chat reads them.
fn room_files(persistent: &PersistentArgs) -> chat::record::WorkspaceFiles {
    attachment_limits(persistent).map_or_else(Default::default, |limits| {
        chat::record::WorkspaceFiles {
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
    files: chat::record::WorkspaceFiles,
    discussions: &[crate::room_discussions::Published],
) -> anyhow::Result<()> {
    if !apps.iter().any(|app| app == crate::room_chat::CHAT_APP) {
        return Ok(());
    }
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let record = tilo_relay::room_bots::workspace_record(&catalogue, crate::room_chat::CHAT_APP);
    workspaces
        .author_application(
            workspace,
            cert,
            &catalogue,
            crate::room_chat::CHAT_APP,
            &record,
            |doc| {
                crate::room_chat::settle_chat(doc, channels)?;
                if let Some(name) = feedback {
                    crate::room_chat::settle_feedback_channel(doc, name)?;
                }
                crate::room_chat::settle_files(doc, files)?;
                crate::room_discussions::settle_threads(doc, discussions).map(drop)
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
    shown: &str,
    strips: &[String],
) -> anyhow::Result<()> {
    if strips.is_empty() {
        return Ok(());
    }
    workspaces
        .author_room_record(workspace, cert, crate::room_strips::DESKTOP, |doc| {
            crate::room_strips::settle_strips(doc, workspace, shown, strips)
        })
        .await
        .map(drop)
        .map_err(|error| anyhow::anyhow!("give the shared desktop its strips: {error}"))
}

/// The apps the workspace's members write through: the ones `--author-app`
/// names, or, naming none, every first-party app, every seeded one, and the
/// workspace's own shared desktop.
fn authored_apps(args: &PersistentArgs) -> Vec<String> {
    if args.author_apps.is_empty() {
        let mut apps: Vec<String> = tilo_desktop_model::first_party::FIRST_PARTY_APPS
            .iter()
            .map(|app| (*app).to_owned())
            .collect();
        // The shared desktop is the workspace's own record rather than some
        // package's: every member writes the desk its windows are on, so the
        // workspace's host lets it author, as it lets the first-party apps
        // author the records their windows show.
        apps.push(tilo_desktop_model::ownership::DESKTOP_APP.to_owned());
        for app in &args.seed_apps {
            if !apps.contains(app) {
                apps.push(app.clone());
            }
        }
        return apps;
    }
    // No app's id is `none`: the flag says the workspace is read-only.
    args.author_apps
        .iter()
        .filter(|app| app.as_str() != NO_AUTHORED_APPS)
        .cloned()
        .collect()
}

/// Refound each `APP/RECORD` the operator named, once per start that names it:
/// a record whose retained history lost the base its deltas were written over
/// is published again as the workspace's next checkpoint of it, and what its
/// channel retained until now is archived beside the replay. What was done is
/// kept in `refounded.json` beside the workspace, so a later start naming the
/// same record does not found it again.
async fn refound_records(
    workspaces: &Workspaces,
    workspace: &str,
    named: &[String],
    state: &Path,
) -> anyhow::Result<()> {
    if named.is_empty() {
        return Ok(());
    }
    let done_at = state.join("refounded.json");
    let done: BTreeMap<String, String> = read_json(&done_at)?;
    let mut done = done;
    for entry in named {
        let (app, record) = entry
            .split_once('/')
            .with_context(|| format!("--refound {entry:?} is not APP/RECORD"))?;
        if done.contains_key(entry) {
            continue;
        }
        let outcome = workspaces
            .refound_record(workspace, app, record)
            .await
            .map_err(|error| anyhow::anyhow!("refound {entry}: {error}"))?;
        tracing::info!(
            workspace = %workspace,
            app = %outcome.app,
            record = %outcome.record,
            archived = outcome.archived,
            "a record was refounded and its old history archived"
        );
        done.insert(
            entry.clone(),
            format!("archived {} entries", outcome.archived),
        );
        write_json(&done_at, &done)?;
    }
    Ok(())
}

/// Make exactly the operator's `apps` authors of the workspace, so its members
/// write through them, and take authorship back from an app this host made
/// an author at an earlier start and no longer names (kept in `authored`).
pub(crate) async fn settle_app_authorship(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    apps: &[String],
    authored: &Path,
) -> anyhow::Result<()> {
    let earlier: BTreeSet<String> = read_json(authored)?;
    let apps: BTreeSet<String> = apps.iter().cloned().collect();
    let withdrawn: BTreeSet<String> = earlier.difference(&apps).cloned().collect();
    workspaces
        .settle_app_authorship(workspace, cert, &apps, &withdrawn)
        .await
        .map_err(|error| anyhow::anyhow!("let members write through {apps:?}: {error}"))?;
    write_json(authored, &apps)?;
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
        .put_room_grants(workspace, cert, request_id, vec![deny])
        .await
        .map_err(|error| anyhow::anyhow!("deny outside effects to guests: {error}"))?;
    Ok(())
}

/// The grant letting the workspace's members (guests are Editors) invite
/// someone, which a workspace taking people by invitation holds and an open one
/// does not: anyone joins an open workspace through its door.
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
            .put_room_grants(workspace, cert, request_id, vec![invite])
            .await
            .map_err(|error| anyhow::anyhow!("let members invite: {error}"))?;
    } else {
        workspaces
            .remove_room_grants(
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
/// asked once: the listing and the door are the workspace's governance from
/// then on, so an operator that names neither leaves them as the workspace's
/// own people settled them, and a restart changes nothing.
async fn settle_listing(
    workspaces: &Workspaces,
    workspace: &str,
    host_cert: &DeviceCert,
    persistent: &PersistentArgs,
) -> anyhow::Result<()> {
    let Some(description) = &persistent.listing_description else {
        return Ok(());
    };
    workspaces
        .publish(
            workspace,
            host_cert,
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
    host_cert: &DeviceCert,
    persistent: &PersistentArgs,
) -> anyhow::Result<()> {
    if !persistent.invitation_only {
        return Ok(());
    }
    workspaces
        .list_invitation_only(workspace, host_cert, true)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    workspaces
        .list_wait_list_max(workspace, host_cert, persistent.wait_list_max)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one startup sequence; the hosted relay lives until shutdown"
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
    let id_file = persistent.room_id_file.clone().context("--room-id-file")?;
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
                as std::sync::Arc<dyn tilo_agent_participant::FeedbackReviewer>
        });
    let host = host_key(&state)?;
    let host_cert = enrol(
        &root,
        &host,
        &format!("{name} workspace host"),
        tilo_user_identity::DeviceCapabilities::host(),
    )?;
    let config = tilo_relay::RelayConfig {
        state_root: tilo_relay_protocol::metrics::room_relay_root(&state),
        identity_root: state.join("identity"),
        member_limits: persistent.member_limits.then(Default::default),
        iroh_listen: persistent
            .iroh_listen
            .unwrap_or_else(|| tilo_relay::RelayConfig::default().iroh_listen),
        ..Default::default()
    };
    let identity = hosted_identity(&root, host, host_cert, &config.state_root)?;
    // A host that places its workspaces' browser sessions certifies them as
    // the person's stand-in, which it makes from the root it holds: nobody
    // else approves for them here.
    if tilo_relay::hosted_browser::HostedBrowsers::from_env()
        .map_err(anyhow::Error::msg)?
        .is_some()
    {
        tilo_relay::hosted_browser::keep_stand_in(&config.state_root, &root)
            .map_err(anyhow::Error::msg)?;
    }
    let hosted = start_relay_lending(
        config,
        identity,
        &args.web,
        &state,
        crate::web_cli::Lent { feedback_reviewer },
    )
    .await?;
    // Everything a start settles and begins, in one place: a step that fails
    // stops the relay it was started on before the error goes up (the `match`
    // below).
    let serving = async {
        let workspaces = hosted
            .relay
            .workspaces()
            .context("the relay governs no workspaces in this mode")?
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
            .offer_own_workspaces(tilo_relay::workspaces::OwnWorkspaceOffer {
                host,
                storage_bytes: persistent.person_storage_gb.saturating_mul(1_000_000_000),
            })
            .map_err(|error| anyhow::anyhow!("read whose own workspaces are whose: {error}"))?;
        let workspace = governed_room(&workspaces, &shown, &id_file, &store, &root).await?;
        // Its own workspaces are for the people this workspace's door, or an
        // invitation into it, let in: never for anyone holding a root alone.
        workspaces.admit_own_through(&workspace);
        // The certificate this host serves the workspace with: a build renamed
        // the host's own after a workspace was made with it, and the host took
        // the record's back (`Workspaces::serving_certificate`). Every governed
        // operation below names it, so a renamed certificate is no refusal.
        let host_cert = workspaces.serving_certificate().clone();
        // The removals moderation made before the door had them, taken over once
        // and kept out for the rest of their day.
        let adopted = workspaces
            .adopt_legacy_removals(&workspace, &state.join("removals.json"))
            .await
            .context("take over the removals the room kept before the door")?;
        if adopted > 0 {
            tracing::info!(%workspace, adopted, "removals taken over from the room's removals file");
        }
        // A setting of the operator's that the workspace's governance refuses
        // stops the start only where going on without it would let in more than
        // the operator configured: the door, the guests' outside effects and the
        // removals above. The rest is said and settled again at the next start.
        settled(
            &workspace,
            settle_listing(&workspaces, &workspace, &host_cert, &persistent).await,
        );
        settle_invitation_only(&workspaces, &workspace, &host_cert, &persistent)
            .await
            .map_err(|error| anyhow::anyhow!("settle the workspace's door: {error}"))?;
        seed_apps(
            &workspaces,
            &workspace,
            &host_cert,
            &persistent.seed_apps,
            &state.join("seeded.json"),
        )
        .await?;
        settle_chat(
            &workspaces,
            &workspace,
            &host_cert,
            &persistent.seed_apps,
            &persistent.chat_channels,
            persistent.feedback_channel.as_deref(),
            room_files(&persistent),
            &persistent.discussions,
        )
        .await?;
        settle_strips(
            &workspaces,
            &workspace,
            &host_cert,
            &shown,
            &persistent.strips,
        )
        .await?;
        ensure_guest_policy(&workspaces, &workspace, &host_cert).await?;
        // Members inviting, or not, never lets in anyone the door does not.
        settled(
            &workspace,
            settle_member_invites(&workspaces, &workspace, &host_cert, persistent.members_only).await,
        );
        settled(
            &workspace,
            settle_app_authorship(
                &workspaces,
                &workspace,
                &host_cert,
                &authored_apps(&persistent),
                &state.join("authored-apps.json"),
            )
            .await,
        );
        // A record the operator named whose retained history lost the base its
        // deltas were written over: founded again from what the workspace still
        // reads, its old history archived beside the replay. Runs once per start
        // that names it; a record that still reads is left as it is.
        refound_records(&workspaces, &workspace, &persistent.refound, &state).await?;
        // The wait list, when the workspace takes people by invitation and
        // keeps one: the addresses' windows and the mail the door sends from
        // its page (the web entry, or the door `--wait-list-mail` names). What
        // the workspace's door keeps is its governance's, settled above.
        let door_policy = workspaces
            .door_policy(&workspace)
            .await
            .unwrap_or_default();
        let settings = door::WaitListSettings::from_env(
            door_policy.wait_list_max,
            persistent
                .wait_list_mail
                .clone()
                .or_else(|| args.web.web_entry.clone()),
            std::env::var_os("TILO_ROOMS_LOCAL").is_some(),
        )
        .context("read the door's mail settings")?;
        let wait_list = door::WaitListDoor::open(&workspaces, &workspace, &shown, &state, &settings)
            .context("open the door's wait list")?;
        let door = wait_list.map(std::sync::Arc::new);
        if let Some(door) = &door {
            let _ = workspaces.offer_wait_list(door.offered());
            tracing::info!(
                workspace = %workspace,
                max = door.max(),
                mail = door.mails(),
                "the door keeps a wait list"
            );
        }
        let mut config = web_config(&args.web, listen, &state);
        config.door.clone_from(&door);
        let web = hosted
            .relay
            .serve_web(config)
            .await
            .context("start the browser listener")?;
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
            }
            .to_line()
        );
        // The moderation agent runs as the workspace's bot for as long as the workspace
        // does; it answers reports left open while the workspace was down first.
        let moderator = moderation.map(|agent| {
            tokio::spawn(
                tilo_relay::room_bots::ModerationBot::new(
                    workspaces.clone(),
                    workspace.clone(),
                    host_cert.clone(),
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
                tilo_relay::public_avatars::PublicAvatars::new(dir.clone(), url)
                    .with_context(|| format!("prepare {}", dir.display()))?,
            ),
            _ => None,
        };
        let connector_host = tilo_relay::room_bots::installed::ConnectorHost::new(
            workers,
            // The relay's content, where it keeps each workspace's files, uploads
            // and what its apps made: one store per workspace for everything this
            // host keeps.
            hosted.relay.content(),
            public,
            tilo_relay::room_bots::discord::Served {
                avatars_url: persistent.public_avatars_url.clone(),
                web_entry: args.web.web_entry.clone(),
                origin: Some(web.origin.clone()),
                discord_api_base: persistent.discord_api_base.clone(),
            },
        );
        let (runners, discord) = start_bots(
            &workspaces,
            &workspace,
            &host_cert,
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
        // The files members post in the workspace's Chat, kept in the same store
        // as what the bots fetch, so they stay once their poster left.
        let posted = connector_host
            .store(&workspace)
            .map_err(|error| anyhow::anyhow!("the workspace's content: {error}"))?;
        let attachments = attachment_limits(&persistent).map(|limits| {
            tokio::spawn(
                tilo_relay::room_bots::attachments::WorkspaceAttachments::new(
                    workspaces.clone(),
                    workspace.clone(),
                    host_cert.device_public_key,
                    posted,
                    state.join("attachments.json"),
                    limits,
                )
                .run(),
            )
        });
        // Bots the workspace's members ask for, beside the operator's.
        let requested =
            tilo_relay::room_bots::requested::spawn(tilo_relay::room_bots::requested::Follower {
                workspaces: workspaces.clone(),
                workspace: workspace.clone(),
                host: host_cert.clone(),
                root: root.clone(),
                state: state.clone(),
                operator: bots.iter().map(|spec| spec.entry.clone()).collect(),
                connectors: Some(connector_host),
            });
        let context = PersistentRoom {
            workspaces,
            workspace,
            host_cert,
            web,
        };
        // The deployment's own routes: its health check and the addresses other
        // services are configured with. Started once the room serves, so a health
        // check that answers means people can get in, and announced at the address
        // it bound so a deployment can reach a port it let the kernel pick.
        let management = match persistent.http {
            Some(listen) => Some(
                http::serve(http::Config {
                    listen,
                    avatars: persistent.public_avatars.clone(),
                    sign_in,
                    web_entry: args.web.web_entry.clone(),
                    discord: discord.clone(),
                })
                .await?,
            ),
            None => None,
        };
        if let Some(management) = &management {
            println!(
                "{}",
                OperatorEvent::DeploymentRoutes {
                    origin: format!("http://{}", management.addr),
                }
                .to_line()
            );
        }
        // The operator's door to the running workspace, for a deployment whose
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
            wait_list: door,
            owner_socket,
        })
    }
    .await;
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
            // A start that failed stops the relay it began before its error
            // goes up: a relay dropped from here closes its endpoint after the
            // runtime is gone, with its tasks still joining what they started.
            hosted.shutdown().await;
            return Err(error);
        }
    };
    let mut invitations = wait_list
        .as_ref()
        .and_then(|door| door.invitations(&context.workspaces));
    let result = tokio::select! {
        result = serve_operator(&context, &mut operator, invitations.as_mut(), owner_socket.as_ref()) => result,
        result = shutdown => result,
    };
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
    /// The task serving the bots members ask for.
    requested: tokio::task::JoinHandle<()>,
    /// The workspace the operator's loop serves.
    context: PersistentRoom,
    /// The deployment's own routes, which stop when dropped.
    management: Option<http::Handle>,
    /// The wait list the door keeps, when the workspace takes people by
    /// invitation and keeps one.
    wait_list: Option<std::sync::Arc<door::WaitListDoor>>,
    /// The socket the operator declares owners through, when it named one.
    owner_socket: Option<tokio::net::UnixListener>,
}

async fn summary(
    context: &PersistentRoom,
) -> anyhow::Result<tilo_relay_session::workspaces::WorkspaceSummary> {
    context
        .workspaces
        .summary(&context.workspace)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// Addresses left at the door and not confirmed in time are let go, also
/// while nobody touches the wait list.
async fn expire_wait_list(context: &PersistentRoom) {
    if context.door().await.wait_list_max == 0 {
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
    async fn observe(&mut self, context: &PersistentRoom) -> anyhow::Result<()> {
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

/// Answer one operator line on the socket: the same command its stdin takes,
/// for a deployment whose workspace runs in a container and so has no stdin to
/// write to.
async fn answer_operator_socket(context: &PersistentRoom, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    let (reading, mut writing) = stream.into_split();
    let mut line = String::new();
    let mut reading = tokio::io::BufReader::new(reading);
    let answer = match reading.read_line(&mut line).await {
        Ok(0) => return,
        Ok(_) => match OperatorCommand::from_line(&line) {
            Ok(OperatorCommand::DeclareOwner { user_public_key }) => {
                declare_owner(
                    &context.workspaces,
                    &context.workspace,
                    &context.host_cert,
                    &user_public_key,
                )
                .await
            }
            Ok(other) => OperatorEvent::OwnerRefused {
                user_public_key: String::new(),
                reason: format!(
                    "the owner socket takes declare_owner, not {}",
                    other.to_line().trim()
                ),
            }
            .to_line(),
            Err(error) => OperatorEvent::OwnerRefused {
                user_public_key: String::new(),
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
    context: &PersistentRoom,
    operator: &mut tokio::sync::mpsc::Receiver<Result<OperatorCommand, String>>,
    mail: Option<&mut door::InvitationMail>,
    owner_socket: Option<&tokio::net::UnixListener>,
) -> anyhow::Result<()> {
    let mut changes = context.workspaces.governance_changes();
    let removals = Removals::load(&summary(context).await?.members);
    let mut presence = OnlineWatch { last_online: None };
    let mut online_tick = tokio::time::interval(Duration::from_secs(operator::ONLINE_EVERY_SECS));
    let mut sweep_tick = tokio::time::interval(Duration::from_secs(WAIT_LIST_SWEEP_SECS));
    let mut mail = mail;
    // A deployment that runs the room without an operator leaves its input at
    // its end: the room then keeps serving, and only a command ends it.
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
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => bail!("the relay stopped governing"),
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
                    answer_operator_socket(context, stream).await;
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
                | OperatorCommand::Standing { .. }))) => {
                    println!("{}", answer_wait_list_command(context, command).await.to_line());
                }
                // An invitation the door mailed itself is acknowledged here:
                // nothing else in the workspace hears of it.
                Some(Ok(OperatorCommand::InviteMailQueued { .. })) => {}
                Some(Ok(OperatorCommand::DeclareOwner { user_public_key })) => {
                    println!(
                        "{}",
                        declare_owner(
                            &context.workspaces,
                            &context.workspace,
                            &context.host_cert,
                            &user_public_key,
                        )
                        .await
                    );
                }
                Some(Ok(OperatorCommand::Admit { .. })) => bail!("a persistent workspace admits guests through its door"),
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
    use tilo_relay_protocol::workspaces::{JoinReply, WorkspacesReply, WorkspacesRequest};
    use tilo_user_identity::DeviceCapabilities;
    const NS_PER_SEC: u64 = 1_000_000_000;

    #[test]
    fn the_workspace_is_named_after_the_room_capitalized() {
        assert_eq!(workspace_name("plaza"), "Plaza");
        assert_eq!(workspace_name(""), "");
    }

    #[test]
    fn the_workspace_authors_every_first_party_app_unless_it_names_apps() {
        let mut args = PersistentArgs {
            seed_apps: vec!["so.tilo.chat".into()],
            ..PersistentArgs::default()
        };
        let every = authored_apps(&args);
        for app in tilo_desktop_model::first_party::FIRST_PARTY_APPS {
            assert!(every.iter().any(|named| named.as_str() == *app), "{app}");
        }
        assert!(every.iter().any(|app| app == "so.tilo.chat"));
        assert_eq!(
            every.len(),
            every.iter().collect::<BTreeSet<_>>().len(),
            "an app is named once"
        );
        args.author_apps = vec!["so.tilo.chat".into()];
        assert_eq!(authored_apps(&args), ["so.tilo.chat"]);
        args.author_apps = vec![NO_AUTHORED_APPS.into()];
        assert!(authored_apps(&args).is_empty(), "a read-only workspace");
    }

    fn store() -> tilo_secrets::SecretStore {
        tilo_secrets::SecretStore::memory(SECRET_APP)
    }

    fn member(key: u8, administrator: bool) -> tilo_peer_state::GovernedMember {
        tilo_peer_state::GovernedMember {
            user_public_key: [key; 32],
            role: tilo_peer_state::join::Role::Editor,
            display_name: None,
            active_device_keys: Vec::new(),
            removed: false,
            administrator,
        }
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
    fn a_mounted_root_must_match_the_rooms_stored_owner() {
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
        let removed = |key: u8, removed: bool| tilo_peer_state::GovernedMember {
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
        let listed = |removed: bool| tilo_peer_state::GovernedMember {
            removed,
            ..member(4, false)
        };
        let mut seen = BTreeSet::new();
        assert_eq!(newly_removed(&mut seen, &[listed(true)]), vec![[4; 32]]);
        assert!(newly_removed(&mut seen, &[listed(false)]).is_empty());
        assert_eq!(newly_removed(&mut seen, &[listed(true)]), vec![[4; 32]]);
    }

    /// A relay governing one workspace `Plaza`, its workspaces and the host's
    /// device certificate.
    async fn plaza(dir: &Path) -> (tilo_relay::daemon::Relay, Workspaces, DeviceCert, String) {
        let (relay, workspaces, host_cert) = host(dir).await;
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        (relay, workspaces, host_cert, workspace)
    }

    /// Seeds as the operator's start does, naming the apps it asked `--seed-app`
    /// for, and answers with what the workspace seeded.
    async fn seeded(
        state: &Path,
        host_cert: &DeviceCert,
        workspaces: &Workspaces,
        workspace: &str,
        apps: &[&str],
    ) -> BTreeSet<String> {
        let seeded_file = state.join("seeded.json");
        seed_apps(
            workspaces,
            workspace,
            host_cert,
            &apps.iter().map(|app| (*app).to_owned()).collect::<Vec<_>>(),
            &seeded_file,
        )
        .await
        .unwrap();
        read_json(&seeded_file).unwrap()
    }

    /// A persistent workspace's first start seeds its apps: a workspace that was
    /// just created holds no channel yet, so each seed's genesis waits for the
    /// group the host's channel service creates for it, and a wait that is too
    /// short stops the start.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fresh_workspace_seeds_its_apps_and_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let (relay, workspaces, host_cert) =
            host_with(dir.path(), tilo_user_identity::DeviceCapabilities::host()).await;
        let workspace = workspaces.create("Plaza").await.unwrap().workspace;
        let apps = ["so.tilo.notes", "so.tilo.chat"];
        let first = seeded(dir.path(), &host_cert, &workspaces, &workspace, &apps).await;
        assert_eq!(
            first,
            apps.iter()
                .map(|app| (*app).to_owned())
                .collect::<BTreeSet<_>>()
        );
        // A restart seeds nothing again: what the first start seeded is kept.
        let again = seeded(dir.path(), &host_cert, &workspaces, &workspace, &apps).await;
        assert_eq!(again, first);
        relay.shutdown().await;
    }

    /// The relay of [`plaza`] started on `dir`, as every start signs its
    /// certificate afresh.
    async fn host(dir: &Path) -> (tilo_relay::daemon::Relay, Workspaces, DeviceCert) {
        host_with(dir, DeviceCapabilities::host()).await
    }

    /// [`host`] whose certificate carries `capabilities`: the host of a
    /// persistent workspace owns its workspaces, so it takes part in their
    /// channels too.
    async fn host_with(
        dir: &Path,
        capabilities: DeviceCapabilities,
    ) -> (tilo_relay::daemon::Relay, Workspaces, DeviceCert) {
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
        let host_cert = enrol(&root, &host, "host", capabilities).unwrap();
        let relay = tilo_relay::daemon::Relay::start(
            tilo_relay::RelayConfig {
                state_root: dir.join("relay"),
                identity_root: dir.join("identity"),
                ..Default::default()
            },
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let workspaces = relay.workspaces().unwrap().clone();
        (relay, workspaces, host_cert)
    }

    /// A host started as `tilo-relay room` starts one: the operator's root
    /// file, the host device it keeps in its state, and the revocation log
    /// under that state.
    async fn start_room(
        root: &SigningKey,
        host: &SigningKey,
        host_cert: &DeviceCert,
        relay_state: &Path,
        state: &Path,
    ) -> tilo_relay::daemon::Relay {
        let identity = hosted_identity(root, host.clone(), host_cert.clone(), relay_state).unwrap();
        tilo_relay::daemon::Relay::start(
            tilo_relay::RelayConfig {
                state_root: relay_state.to_path_buf(),
                identity_root: state.join("identity"),
                ..Default::default()
            },
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
        let relay_state = tilo_relay_protocol::metrics::room_relay_root(&state);
        let root = SigningKey::from_bytes(&[0x11; 32]);
        let host = SigningKey::from_bytes(&[0x22; 32]);
        let host_cert = enrol(&root, &host, "host", DeviceCapabilities::host()).unwrap();
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
        let relay = start_room(&root, &host, &host_cert, &relay_state, &state).await;
        let addr = relay.node_addr();
        let admitted = tilo_relay::RelayClient::connect(
            endpoint().await,
            addr,
            user,
            leaked_cert.clone(),
            leaked.clone(),
            RevocationList::new(),
        )
        .await;
        assert!(admitted.is_ok(), "an unrecognised device connects");
        relay.shutdown().await;

        // The operator's revocation, written where the host reads it.
        let (_, changed) = tilo_relay::root_signer::revoke_device(
            &relay_state,
            &root,
            leaked_cert.device_public_key,
            tilo_user_identity::RevocationReason::Compromised,
        )
        .unwrap();
        assert!(changed, "a first revocation changes the log");

        let relay = start_room(&root, &host, &host_cert, &relay_state, &state).await;
        let addr = relay.node_addr();
        let refused = tilo_relay::RelayClient::connect(
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
        let admitted = tilo_relay::RelayClient::connect(
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
            .page_rooms(
                device,
                WorkspacesRequest::Invite {
                    workspace: workspace.to_owned(),
                    request_id: rand::random(),
                    role: Role::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3600 * NS_PER_SEC),
                    inviter_name: Some("Member".into()),
                    wait_list_entry: None,
                    recipient: None,
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

    /// Whether `person`'s device redeems the door's `invitation` and `token`,
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

    /// An owner the operator declares gets in through a door that takes
    /// people by invitation, and governs the workspace: the declaration is the
    /// workspace's own governance, so it is there before their device arrives
    /// and stays after a restart.
    #[tokio::test]
    async fn a_declared_owner_is_let_in_and_governs_from_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let (relay, workspaces, host_cert, workspace) = plaza(dir.path()).await;
        ensure_guest_policy(&workspaces, &workspace, &host_cert)
            .await
            .unwrap();
        settle_member_invites(&workspaces, &workspace, &host_cert, false)
            .await
            .unwrap();
        workspaces
            .publish(
                &workspace,
                &host_cert,
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
            &host_cert,
            &PersistentArgs {
                invitation_only: true,
                ..PersistentArgs::default()
            },
        )
        .await
        .unwrap();

        // Nobody is in the workspace yet, so this is how its first person
        // gets in: the door takes people by invitation, and the operator
        // declares them.
        let owner = guest(31);
        let answered = declare_owner(
            &workspaces,
            &workspace,
            &host_cert,
            &hex::encode(owner.1.user_public_key),
        )
        .await;
        assert!(answered.contains("owner_declared"), "{answered}");
        // A stranger is turned away: the door takes people by invitation, and
        // the declaration is what the workspace holds of the person it named.
        let (_, stranger) = guest(32);
        assert!(
            workspaces
                .admit_listed(
                    &workspace,
                    &tilo_relay_protocol::directory::DoorJoin {
                        user_public_key: hex::encode(stranger.user_public_key),
                        display_name: "Eve".into(),
                    },
                )
                .await
                .is_err(),
            "a stranger holds no invitation and is in no declaration"
        );
        // In they walk, through the invitation-only door.
        let (issued, token) = workspaces
            .admit_listed(
                &workspace,
                &tilo_relay_protocol::directory::DoorJoin {
                    user_public_key: hex::encode(owner.1.user_public_key),
                    display_name: "Ada".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            walk_in(&workspaces, &owner, issued.signed_invitation, &token).await,
            "the door lets a declared person in"
        );
        let members = workspaces.summary(&workspace).await.unwrap().members;
        assert!(
            members
                .iter()
                .any(|member| member.user_public_key == owner.1.user_public_key
                    && member.administrator
                    && !member.removed),
            "the declared owner governs the workspace once their device is in"
        );
        // What the operator cannot declare is nothing at all.
        let refused = declare_owner(&workspaces, &workspace, &host_cert, "Ada").await;
        assert!(refused.contains("owner_refused"), "{refused}");
        relay.shutdown().await;
    }

    /// Someone the listed workspace's door lets in is a member; once the
    /// workspace takes people by invitation, a member invites someone with an
    /// invitation of their own, which admits them by itself, while the
    /// workspace's link is no invitation at all. Once it lets anyone in again,
    /// members no longer invite.
    #[tokio::test]
    async fn taking_people_by_invitation_the_invitation_admits_the_guest() {
        let dir = tempfile::tempdir().unwrap();
        let (relay, workspaces, host_cert, workspace) = plaza(dir.path()).await;
        ensure_guest_policy(&workspaces, &workspace, &host_cert)
            .await
            .unwrap();
        // An open workspace settles as one at every start, holding no such grant.
        settle_member_invites(&workspaces, &workspace, &host_cert, false)
            .await
            .unwrap();

        // Someone who joined through the door while the workspace was listed
        // is a member. A listed workspace's link is its invitation: members
        // invite nobody themselves.
        workspaces
            .publish(
                &workspace,
                &host_cert,
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
                &tilo_relay_protocol::directory::DoorJoin {
                    user_public_key: hex::encode(member.1.user_public_key),
                    display_name: "Ada".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            walk_in(&workspaces, &member, issued.signed_invitation, &token).await,
            "the door lets the first person in"
        );
        let asked = invite(&workspaces, &workspace, &member.1).await;
        assert!(matches!(asked, WorkspacesReply::Refused(_)), "{asked:?}");

        workspaces.unpublish(&workspace, &host_cert).await.unwrap();
        settle_member_invites(&workspaces, &workspace, &host_cert, true)
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
            members.iter().any(
                |member| member.user_public_key == stranger.1.user_public_key && !member.removed
            ),
            "the invited stranger is a member"
        );

        settle_member_invites(&workspaces, &workspace, &host_cert, false)
            .await
            .unwrap();
        let asked = invite(&workspaces, &workspace, &member.1).await;
        assert!(matches!(asked, WorkspacesReply::Refused(_)), "{asked:?}");
        relay.shutdown().await;
    }

    /// A listed workspace people walked in through, with an owner declared,
    /// a moderator and someone removed, takes its host's start-time settings
    /// again after the host restarts, as the public room does at every start.
    #[tokio::test]
    async fn a_lived_in_workspace_takes_its_hosts_settings_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (relay, workspaces, host_cert, workspace) = plaza(dir.path()).await;
        ensure_guest_policy(&workspaces, &workspace, &host_cert)
            .await
            .unwrap();
        settle_member_invites(&workspaces, &workspace, &host_cert, false)
            .await
            .unwrap();
        workspaces
            .publish(
                &workspace,
                &host_cert,
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
                    &tilo_relay_protocol::directory::DoorJoin {
                        user_public_key: hex::encode(person.1.user_public_key),
                        display_name: "Guest".into(),
                    },
                )
                .await
                .unwrap();
            assert!(walk_in(&workspaces, person, issued.signed_invitation, &token).await);
        }
        workspaces
            .set_member_administrator(&workspace, &host_cert, [1; 32], people[0].1.user_public_key)
            .await
            .unwrap();
        workspaces
            .set_member_role(
                &workspace,
                &host_cert,
                tilo_relay_session::workspaces::SetMemberRoleRequest {
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
                &host_cert,
                tilo_relay_session::workspaces::RemoveMemberRequest {
                    request_id: [3; 32],
                    user_public_key: people[2].1.user_public_key,
                },
            )
            .await
            .unwrap();
        relay.shutdown().await;

        let (relay, workspaces, host_cert) = host(dir.path()).await;
        for step in 0..2 {
            ensure_guest_policy(&workspaces, &workspace, &host_cert)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_member_invites(&workspaces, &workspace, &host_cert, false)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
            settle_member_invites(&workspaces, &workspace, &host_cert, true)
                .await
                .unwrap_or_else(|error| panic!("start {step}: {error:#}"));
        }
        let log = workspaces.membership_log(&workspace).await.unwrap();
        assert!(
            !log.iter().any(|line| line.contains("log against rows")),
            "{log:#?}"
        );
        relay.shutdown().await;
    }

    #[test]
    fn a_host_keeps_ten_gigabytes_per_person_unless_the_operator_says_otherwise() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Args {
            #[command(flatten)]
            persistent: PersistentArgs,
        }
        let serving = |extra: &[&str]| {
            Args::try_parse_from(
                [
                    "relay",
                    "--persistent",
                    "tilo",
                    "--root-file",
                    "/run/secrets/root",
                    "--room-id-file",
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
            Args::try_parse_from(["relay", "--person-storage-gb", "40"]).is_err(),
            "workspaces of people's own are kept by a persistent host"
        );
    }

    #[test]
    fn a_bot_spec_names_an_entry_and_its_inputs() {
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
