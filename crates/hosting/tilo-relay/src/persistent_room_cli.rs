//! `tilo-relay room --persistent`: one always-on public room. The room's
//! owner root comes from the operator's encrypted secrets (mounted as a
//! file) and is kept in this host's secret store, never in a record, the
//! environment or a log. This is the documented exception to "the host
//! never holds a room's root": nobody is present to approve guests, and
//! the room is public anyway. Each guest is pre-admitted as their own
//! person; the operator (tilo-room-service) decides who may join.
//!
//! Visitors who only watch (a page embedding the room) are not members one
//! by one: they all act as the room's one spectator person, a Viewer whose
//! root this host also keeps, with short-lived device certificates that
//! carry no capabilities.

#![allow(
    clippy::similar_names,
    reason = "a room and its root are the two things this module is about"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_relay::daemon::RelayIdentity;
use tilo_relay::rooms::{PreAdmitRequest, Rooms, WaitListed};
use tilo_relay::web::WebHandle;
use tilo_relay_protocol::room_operator::{self, OperatorCommand, RoomEvent, hex32};
use tilo_relay_protocol::rooms::{JoinReply, JoinStep, PreAdmission, RoomRole, join_proof_digest};
use tilo_relay_session::rooms::SetMemberRoleRequest;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use crate::web_cli::{RoomArgs, enrol, operator_lines, start_relay_lending, web_config};

/// The secret store this host keeps room roots in.
pub(crate) const SECRET_APP: &str = "so.tilo.relay";
/// How long a pre-admitted invitation stays redeemable: a visitor who
/// closes the tab leaves nothing redeemable behind for long.
pub(crate) const PRE_ADMISSION_SECS: u64 = 600;
/// How long a spectator's device certificate is valid. The embedding page
/// asks again on its next load; nothing about a spectator is kept.
pub(crate) const SPECTATOR_CERT_SECS: u64 = 3600;
/// The member-list name of the spectator person.
pub(crate) const SPECTATOR_NAME: &str = "Watching visitors";
const MINUTE_MS: u64 = 60 * 1000;
/// Guests pruned in one governance change: the room is held while it
/// commits, and each removal costs time in proportion to its members.
const PRUNE_BATCH: usize = 100;
const NS_PER_SEC: u64 = 1_000_000_000;

#[derive(clap::Args, Clone, Debug, Default)]
pub struct PersistentArgs {
    /// Run one persistent public room with this name instead of a
    /// short-lived one.
    #[arg(long, value_name = "NAME", requires_all = ["root_file", "room_id_file"])]
    pub persistent: Option<String>,
    /// The name people see for the persistent room; its operator name with
    /// a capital first letter when absent. A room already created under
    /// another name is renamed at start.
    #[arg(long = "workspace-name", value_name = "NAME", requires = "persistent")]
    pub workspace_name: Option<String>,
    /// A channel the room's Chat opens with while it has none, by name
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
    /// The channel, by name, the room's members report problems to: kept
    /// in the room's Chat on every start, after its other channels, since
    /// the site's pages send reports to it by its id (`c_<name>`).
    #[arg(
        long = "feedback-channel",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::room_chat::channel_name
    )]
    pub feedback_channel: Option<String>,
    /// A strip, by name, the room's shared desktop has in its first
    /// workspace; repeatable, in order. Added at start where missing; the
    /// strips people made are kept.
    #[arg(
        long = "strip",
        value_name = "NAME",
        requires = "persistent",
        value_parser = crate::room_strips::strip_name
    )]
    pub strips: Vec<String>,
    /// A text published outside the room, announced so its readers find a
    /// discussion thread to arrive in: `<address>=<title>`, the address of
    /// the text and the title its thread is named; repeatable. A text whose
    /// thread the room keeps is left as it is.
    #[arg(
        long = "discussion",
        value_name = "ADDRESS=TITLE",
        requires = "persistent",
        value_parser = crate::room_discussions::parse
    )]
    pub discussions: Vec<crate::room_discussions::Published>,
    /// The room's owner root: a file of 64 hex characters (the Ed25519
    /// seed), mounted from the operator's encrypted secrets.
    #[arg(long = "root-file", value_name = "PATH")]
    pub root_file: Option<PathBuf>,
    /// Where the room's UUID is kept across restarts.
    #[arg(long = "room-id-file", value_name = "PATH")]
    pub room_id_file: Option<PathBuf>,
    /// Apply per-person change and content budgets to members.
    #[arg(long = "member-limits")]
    pub member_limits: bool,
    /// Visitors watching the room at once. Each watching page reads the
    /// room as its spectator person and writes nothing; past this many the
    /// relay refuses the next one's subscription.
    #[arg(long = "max-spectators", default_value_t = 1000)]
    pub max_spectators: usize,
    /// A person (user public key, hex) made an Owner of the room once they
    /// joined; repeatable. A key removed from the list loses the role.
    #[arg(long = "owner", value_name = "HEX")]
    pub owners: Vec<String>,
    /// A person made a Moderator once they joined; repeatable.
    #[arg(long = "moderator", value_name = "HEX")]
    pub moderators: Vec<String>,
    /// Take people by invitation: the door pre-admits only the room's
    /// members (another tab of theirs is another device), and members may
    /// invite someone with an invitation of their own, which they let in.
    /// Settled at every start.
    #[arg(long = "members-only")]
    pub members_only: bool,
    /// How many requests left at the door the room keeps while it takes
    /// people by invitation: each is an address (an email) someone who asked
    /// without an invitation leaves, on the room's wait list for whoever
    /// may invite until they take it off. `0`: the door keeps none.
    #[arg(
        long = "wait-list-max",
        value_name = "COUNT",
        default_value_t = 0,
        requires = "members_only"
    )]
    pub wait_list_max: usize,
    /// Whoever runs the room sends mail from the web entry DOOR (a URL
    /// ending in `/`): members may ask it to mail an invitation on that
    /// door to a confirmed address on the wait list, which it hears as an
    /// `invite_mail` event.
    #[arg(
        long = "wait-list-mail",
        value_name = "DOOR",
        requires = "wait_list_max"
    )]
    pub wait_list_mail: Option<String>,
    /// A file of more owners and moderators, `{"owners": [hex],
    /// "moderators": [hex]}`, read again whenever it changes, so a role is
    /// granted or taken back while the room runs.
    #[arg(long = "roles-file", value_name = "PATH")]
    pub roles_file: Option<PathBuf>,
    /// An app authored into the room at its first start; repeatable.
    #[arg(long = "seed-app", value_name = "APP_ID")]
    pub seed_apps: Vec<String>,
    /// Whether the room shares its members' windows and strips: `personal`
    /// (each member's stay on their device; the room shares its records and
    /// people) or `shared`. Settled at every start.
    #[arg(
        long = "layout",
        value_name = "SCOPE",
        default_value = "personal",
        value_parser = parse_layout
    )]
    pub layout: tilo_desktop_model::projection::RoomLayout,
    /// An app the room's members write through: it authors the room's
    /// content, so a member whose role lets them change the room (a guest
    /// is an Editor) can talk in its Chat; repeatable. Settled at every
    /// start: an app named here no longer stops authoring the room.
    #[arg(long = "author-app", value_name = "APP_ID")]
    pub author_apps: Vec<String>,
    /// A member with no session for this many days stops being one; they
    /// join again through the door as the same person. Membership persists:
    /// a person stays a member until they leave, the host removes them, or
    /// they have been away this long. Being offline for an hour removes
    /// nobody; who is here now is the room's presence, not its membership.
    #[arg(long = "inactive-after-days", default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=3650))]
    pub inactive_after_days: u64,
    /// What to install at the room's first start, approved by the room's
    /// root, with its inputs (`<source> key=value …`); repeatable. The source
    /// is a bot directory entry, or a program file on this host, which the
    /// room runs under the program's own name.
    #[arg(long = "bot", value_name = "SPEC")]
    pub bot_specs: Vec<String>,
    /// The Discord bot token, mounted from the operator's encrypted
    /// secrets; kept where the Discord connector's worker asks for it.
    #[arg(long = "discord-token-file", value_name = "PATH")]
    pub discord_token_file: Option<PathBuf>,
    /// The worker binary of a connector (`APP=PATH`), where it is not
    /// `<name>-worker` beside this binary; repeatable.
    #[arg(long = "connector-worker", value_name = "APP=PATH")]
    pub connector_workers: Vec<String>,
    /// The model API key the room's moderation agent asks with, mounted
    /// from the operator's encrypted secrets; kept in this host's secret
    /// store. Without it the room has no moderation agent and reports wait
    /// for its human Moderators. The same model looks over the reports its
    /// members write in the feedback channel before they send them, asked
    /// through the room, so their pages never hold the key.
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
    /// The largest file a member posts in the room's Chat that the room
    /// keeps once they left, in MiB; a larger one stays with its poster.
    #[arg(long = "attachment-max-mib", default_value_t = 25, value_parser = clap::value_parser!(u64).range(0..=64))]
    pub attachment_max_mib: u64,
    /// What one member's kept files may take together, in MiB; past it
    /// their oldest go first. The room takes in no more of one member's
    /// files in an hour either.
    #[arg(long = "attachment-member-mib", default_value_t = 250)]
    pub attachment_member_mib: u64,
    /// What all members' kept files may take together, in MiB; past it the
    /// oldest go first. `0` keeps none.
    #[arg(long = "attachment-room-mib", default_value_t = 2048)]
    pub attachment_room_mib: u64,
}

/// What every part of the persistent room works with.
pub(crate) struct PersistentRoom {
    pub rooms: Rooms,
    pub room: String,
    pub root: SigningKey,
    pub spectator: SigningKey,
    pub host_cert: DeviceCert,
    pub web: WebHandle,
}

/// The workspace name people see for the persistent room `name` (a lowercase
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
            "persistent mode refuses to start without its room root ({})",
            path.display()
        )
    })?;
    let seed =
        hex32(text.trim()).context("the room root file must hold 64 lowercase hex characters")?;
    Ok(SigningKey::from_bytes(&seed))
}

pub(crate) fn keep_root(
    store: &tilo_secrets::SecretStore,
    room: &str,
    root: &SigningKey,
) -> anyhow::Result<()> {
    store
        .item("room-root", room)
        .put_blocking(&root.to_bytes())
        .context("keep the room root in the host's secret store")
}

pub(crate) fn check_stored_root(
    store: &tilo_secrets::SecretStore,
    room: &str,
    root: &SigningKey,
) -> anyhow::Result<()> {
    let stored = store
        .item("room-root", room)
        .get_blocking()
        .context("read the room root from the host's secret store")?
        .context("this room's owner root is missing from the host's secret store")?;
    // A different mounted root would mint a different owner; refuse rather
    // than govern the room with a key it does not know.
    ensure!(
        stored.expose() == root.to_bytes(),
        "the mounted room root is not this room's owner"
    );
    Ok(())
}

/// The spectator person's root: generated at the room's first start and
/// kept beside the owner root, so every restart serves the same person.
pub(crate) fn spectator_root(
    store: &tilo_secrets::SecretStore,
    room: &str,
) -> anyhow::Result<SigningKey> {
    let item = store.item("spectator-root", room);
    if let Some(stored) = item
        .get_blocking()
        .context("read the spectator root from the host's secret store")?
    {
        let seed = <[u8; 32]>::try_from(stored.expose())
            .map_err(|_| anyhow::anyhow!("the stored spectator root is corrupt"))?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    let root = SigningKey::from_bytes(&rand::random());
    item.put_blocking(&root.to_bytes())
        .context("keep the spectator root in the host's secret store")?;
    Ok(root)
}

/// A certificate for one watching browser's in-memory device key: the
/// spectator person, valid for [`SPECTATOR_CERT_SECS`], no capabilities.
pub(crate) fn spectator_cert(
    spectator: &SigningKey,
    device_public_key: [u8; 32],
    now_ns: u64,
) -> anyhow::Result<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: spectator.verifying_key().to_bytes(),
        device_public_key,
        device_label: "Watching visitor".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: now_ns,
        expires_at_ns: Some(now_ns.saturating_add(SPECTATOR_CERT_SECS * NS_PER_SEC)),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, spectator).context("sign a spectator certificate")?;
    Ok(cert)
}

pub(crate) fn pre_admission(
    root: &SigningKey,
    room: &str,
    token_sha256: [u8; 32],
    label: &str,
    role: RoomRole,
    now_ns: u64,
) -> anyhow::Result<PreAdmission> {
    let mut grant = PreAdmission {
        room: room.to_owned(),
        role,
        token_sha256,
        label: label.to_owned(),
        expires_at_ns: now_ns.saturating_add(PRE_ADMISSION_SECS * NS_PER_SEC),
        owner_user_public_key: root.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = root
        .sign(&grant.digest().map_err(anyhow::Error::msg)?)
        .to_bytes();
    Ok(grant)
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RoleChange {
    Administrator([u8; 32]),
    Moderator([u8; 32]),
    Editor([u8; 32]),
}

/// What makes the member list match the declared lists: promote declared
/// people who joined and do not hold their role, demote people declared
/// before and no longer listed. People not yet in the room wait until they
/// join; the governance change of their join triggers this again.
pub(crate) fn role_changes(
    members: &[tilo_peer_state::GovernedMember],
    owners: &BTreeSet<[u8; 32]>,
    moderators: &BTreeSet<[u8; 32]>,
    previously_declared: &BTreeSet<[u8; 32]>,
) -> Vec<RoleChange> {
    let mut changes = Vec::new();
    for member in members.iter().filter(|member| !member.removed) {
        let key = member.user_public_key;
        if owners.contains(&key) {
            if !member.administrator {
                changes.push(RoleChange::Administrator(key));
            }
        } else if moderators.contains(&key) {
            if !is_moderator(member) {
                changes.push(RoleChange::Moderator(key));
            }
        } else if previously_declared.contains(&key)
            && (member.administrator || is_moderator(member))
        {
            changes.push(RoleChange::Editor(key));
        }
    }
    changes
}

fn is_moderator(member: &tilo_peer_state::GovernedMember) -> bool {
    member.role == tilo_peer_state::join::MemberRole::Moderator
}

/// Guests (not declared, not an administrator, not the spectator person)
/// whose last session is older than `days`. A member with no record yet is
/// recorded as seen now by the caller and never pruned on the same pass.
pub(crate) fn idle_guests(
    members: &[tilo_peer_state::GovernedMember],
    last_seen: &BTreeMap<[u8; 32], u64>,
    keep: &BTreeSet<[u8; 32]>,
    now_ms: u64,
    minutes: u64,
) -> Vec<[u8; 32]> {
    let horizon = now_ms.saturating_sub(minutes.saturating_mul(MINUTE_MS));
    members
        .iter()
        .filter(|member| {
            !member.removed && !member.administrator && !keep.contains(&member.user_public_key)
        })
        .filter(|member| {
            last_seen
                .get(&member.user_public_key)
                .is_some_and(|seen| *seen < horizon)
        })
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

fn token_digest(token: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(token).into()
}

/// `--layout`: `shared` or `personal`.
fn parse_layout(scope: &str) -> Result<tilo_desktop_model::projection::RoomLayout, String> {
    tilo_desktop_model::projection::RoomLayout::from_id(scope)
        .ok_or_else(|| format!("{scope:?} is not a layout scope: say shared or personal"))
}

fn parse_keys(flag: &str, keys: &[String]) -> anyhow::Result<BTreeSet<[u8; 32]>> {
    keys.iter()
        .map(|key| {
            hex32(key)
                .with_context(|| format!("--{flag} {key:?} is not 64 lowercase hex characters"))
        })
        .collect()
}

/// The relay of the persistent room `name` whose `room --state` is `state`
/// and whose root key is in `root_file`, as that room starts it: its state
/// root and identity. Refuses a state whose host has no device key yet.
pub(crate) fn room_relay(
    state: &Path,
    root_file: &Path,
    name: &str,
) -> anyhow::Result<(PathBuf, RelayIdentity)> {
    let root = read_root_file(root_file)?;
    anyhow::ensure!(
        state.join("host-device.key").is_file(),
        "{} holds no room host",
        state.display()
    );
    let host = host_key(state)?;
    let host_cert = enrol(
        &root,
        &host,
        &format!("{name} room host"),
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    )?;
    Ok((
        tilo_relay_protocol::metrics::room_relay_root(state),
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host,
            device_cert: host_cert,
            revocations: RevocationList::new(),
        },
    ))
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

/// The governed room: the one named in `room_id_file`, else the only room
/// this relay governs (a start interrupted between keeping the root and
/// writing the file), else a new one.
async fn governed_room(
    rooms: &Rooms,
    shown: &str,
    id_file: &Path,
    store: &tilo_secrets::SecretStore,
    root: &SigningKey,
) -> anyhow::Result<String> {
    let known = match std::fs::read_to_string(id_file) {
        Ok(text) => Some(text.trim().to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read the room id file"),
    };
    let room = match known {
        Some(room) => room,
        None => match rooms.list().await.as_slice() {
            [only] => only.room.clone(),
            [] => {
                // The root goes into the store before the room exists, so a
                // crash in between is re-adopted above, never orphaned.
                let created = rooms.create(shown).await.map_err(anyhow::Error::msg)?;
                keep_root(store, &created.room, root)?;
                created.room
            }
            _ => bail!("this relay governs several rooms; a persistent room governs one"),
        },
    };
    check_stored_root(store, &room, root)?;
    rooms
        .rename(&room, shown)
        .await
        .map_err(|error| anyhow::anyhow!("name the room {shown:?}: {error}"))?;
    write_atomic(id_file, room.as_bytes(), false).context("write the room id file")?;
    Ok(room)
}

async fn issue_pre_admission(
    rooms: &Rooms,
    host_cert: &DeviceCert,
    root: &SigningKey,
    room: &str,
    user: [u8; 32],
    digest: [u8; 32],
    label: &str,
    role: RoomRole,
) -> anyhow::Result<tilo_relay_session::rooms::IssuedInvitation> {
    let grant = pre_admission(root, room, digest, label, role, tilo_clock::unix_now_ns())?;
    let request = PreAdmitRequest {
        // One token, one invitation: a repeated command is a retry.
        request_id: *blake3::hash(&digest).as_bytes(),
        grant,
        recipient_user_public_key: Some(user),
        inviter_name: Some("Tilo".into()),
    };
    rooms
        .pre_admit(host_cert, request)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// What answering one pre-admission needs, owned, so the room answers a
/// crowd's side by side rather than one after another.
#[derive(Clone)]
struct Admitter {
    rooms: Rooms,
    host_cert: DeviceCert,
    root: SigningKey,
    room: String,
    spectator: [u8; 32],
    removals: Removals,
    /// The door lets in only the room's members (`--members-only`).
    members_only: bool,
}

impl Admitter {
    fn of(context: &PersistentRoom, removals: Removals, members_only: bool) -> Self {
        Self {
            rooms: context.rooms.clone(),
            host_cert: context.host_cert.clone(),
            root: context.root.clone(),
            room: context.room.clone(),
            spectator: context.spectator.verifying_key().to_bytes(),
            removals,
            members_only,
        }
    }
}

/// How long a person moderation removed is refused pre-admission: the day
/// the room service's door also turns away the address they joined from.
const REMOVED_FOR_MS: u64 = 24 * 3600 * 1000;

/// Whom the room has taken in as removed, and when moderation removed each
/// person, kept in the room's state so a restart lets nobody back early.
/// Shared by the operator loop, which takes removals in, and every
/// pre-admission, which may run before the loop has.
#[derive(Clone)]
struct Removals {
    file: PathBuf,
    state: Arc<std::sync::Mutex<RemovalState>>,
}

struct RemovalState {
    /// Every person accounted for as removed, by moderation or pruned after
    /// an absence, so each removal is reported once.
    seen: BTreeSet<[u8; 32]>,
    /// When moderation removed each person, in Unix milliseconds.
    at: BTreeMap<[u8; 32], u64>,
}

impl Removals {
    /// The removals of a room whose members are `members` now: everyone
    /// listed as removed is accounted for, and the removal times kept in
    /// `file` that are still within the day hold.
    fn load(
        file: PathBuf,
        members: &[tilo_peer_state::GovernedMember],
        now_ms: u64,
    ) -> anyhow::Result<Self> {
        let stored: BTreeMap<String, u64> = read_json(&file)?;
        let at = stored
            .iter()
            .filter(|(_, at)| now_ms.saturating_sub(**at) < REMOVED_FOR_MS)
            .filter_map(|(key, at)| hex32(key).map(|key| (key, *at)))
            .collect();
        let seen = members
            .iter()
            .filter(|member| member.removed)
            .map(|member| member.user_public_key)
            .collect();
        Ok(Self {
            file,
            state: Arc::new(std::sync::Mutex::new(RemovalState { seen, at })),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RemovalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Guests about to be pruned: their removal is not moderation's.
    fn pruning(&self, people: &BTreeSet<[u8; 32]>) {
        self.lock().seen.extend(people.iter().copied());
    }

    /// Guests whose pruning failed: still members.
    fn not_pruned(&self, people: &BTreeSet<[u8; 32]>) {
        let mut state = self.lock();
        for person in people {
            state.seen.remove(person);
        }
    }

    /// The people moderation removed since the last call, noted as removed
    /// now and written down.
    fn taken_in(
        &self,
        members: &[tilo_peer_state::GovernedMember],
        now_ms: u64,
    ) -> anyhow::Result<Vec<[u8; 32]>> {
        let mut state = self.lock();
        let fresh = newly_removed(&mut state.seen, members);
        if fresh.is_empty() {
            return Ok(fresh);
        }
        state
            .at
            .retain(|_, at| now_ms.saturating_sub(*at) < REMOVED_FOR_MS);
        for person in &fresh {
            state.at.insert(*person, now_ms);
        }
        let stored: BTreeMap<String, u64> = state
            .at
            .iter()
            .map(|(key, at)| (hex::encode(key), *at))
            .collect();
        drop(state);
        write_json(&self.file, &stored)?;
        Ok(fresh)
    }

    /// Whether `person` is kept out: moderation removed them within the
    /// day, or the room lists them as removed and the loop has not taken
    /// that in yet (a removed guest's page asks again the moment the room
    /// drops it, which may be before the loop hears of the change).
    fn refuses(
        &self,
        person: [u8; 32],
        members: &[tilo_peer_state::GovernedMember],
        now_ms: u64,
    ) -> bool {
        let state = self.lock();
        if state
            .at
            .get(&person)
            .is_some_and(|at| now_ms.saturating_sub(*at) < REMOVED_FOR_MS)
        {
            return true;
        }
        !state.seen.contains(&person)
            && members
                .iter()
                .any(|member| member.user_public_key == person && member.removed)
    }
}

/// Pre-admissions the room answers at once. The room service waits a few
/// seconds for each answer before it turns the visitor into a spectator;
/// answered one at a time, a launch's crowd queued behind each other's
/// writes, while side by side the relay writes everything staged meanwhile
/// in one go. Past this many, the next commands wait their turn.
const PRE_ADMITTING_AT_ONCE: usize = 64;

async fn answer_pre_admit(
    context: Admitter,
    user_public_key: String,
    token_sha256: String,
    label: String,
    role: RoomRole,
) -> RoomEvent {
    let refused = |reason: String| RoomEvent::PreAdmitRefused {
        token_sha256: token_sha256.clone(),
        reason,
        removed: false,
        invite_only: false,
    };
    let (Some(user), Some(digest)) = (hex32(&user_public_key), hex32(&token_sha256)) else {
        return refused("the guest key and token digest must be 64 hex characters".into());
    };
    if user == context.spectator {
        return refused("the spectator person joins no other way".into());
    }
    let members = match context.rooms.summary(&context.room).await {
        Ok(summary) => summary.members,
        Err(error) => return refused(format!("{error}")),
    };
    if context
        .removals
        .refuses(user, &members, tilo_clock::unix_now_ms())
    {
        return RoomEvent::PreAdmitRefused {
            token_sha256,
            reason: "the person was removed from the room within the day".into(),
            removed: true,
            invite_only: false,
        };
    }
    // Taking people by invitation, the door still lets a member in again
    // (another tab of theirs is another device), and nobody else.
    if context.members_only
        && !members
            .iter()
            .any(|member| member.user_public_key == user && !member.removed)
    {
        return RoomEvent::PreAdmitRefused {
            token_sha256,
            reason: "the room takes people by invitation only".into(),
            removed: false,
            invite_only: true,
        };
    }
    match issue_pre_admission(
        &context.rooms,
        &context.host_cert,
        &context.root,
        &context.room,
        user,
        digest,
        &label,
        role,
    )
    .await
    {
        Ok(issued) => RoomEvent::PreAdmitted {
            token_sha256,
            invitation: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(issued.signed_invitation),
        },
        Err(error) => refused(format!("{error:#}")),
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
/// the room takes people by invitation and keeps a wait list.
async fn answer_wait_list(
    context: &PersistentRoom,
    governance: &Governance,
    ask: WaitListAsk,
) -> RoomEvent {
    let WaitListAsk {
        request,
        contact,
        removal_sha256,
        confirm_sha256,
        language,
    } = ask;
    let refused = |reason: String, full: bool| RoomEvent::WaitListRefused {
        request: request.clone(),
        reason,
        full,
    };
    if !governance.members_only || governance.wait_list_max == 0 {
        return refused("the room keeps no wait list".into(), false);
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
    let secrets = tilo_relay::rooms::WaitListSecrets { removal, confirm };
    match context
        .rooms
        .wait(
            &context.room,
            &contact,
            secrets,
            &language,
            governance.wait_list_max,
        )
        .await
    {
        Ok(WaitListed::Added { mail }) => RoomEvent::WaitListed {
            request,
            added: true,
            mail,
        },
        Ok(WaitListed::Already { mail }) => RoomEvent::WaitListed {
            request,
            added: false,
            mail,
        },
        Ok(WaitListed::Full) => refused("the wait list is full".into(), true),
        Err(error) => refused(error.to_string(), false),
    }
}

fn mailbox(mailbox: tilo_relay::rooms::Mailbox) -> room_operator::WaitListMailbox {
    room_operator::WaitListMailbox {
        contact: mailbox.contact,
        language: mailbox.language,
    }
}

/// Take off the wait list the entry whose removal secret hashes to
/// `removal_sha256`, for whoever left it: answered alike whether or not
/// one did.
async fn answer_unlist(
    context: &PersistentRoom,
    request: String,
    removal_sha256: &str,
) -> RoomEvent {
    let Some(removal) = hex32(removal_sha256) else {
        return RoomEvent::WaitListFailed {
            request,
            reason: "the removal digest must be 64 hex characters".into(),
        };
    };
    match context.rooms.leave_wait_list(&context.room, removal).await {
        Ok(told) => RoomEvent::Unlisted {
            request,
            told: told.map(mailbox),
        },
        Err(error) => RoomEvent::WaitListFailed {
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
) -> RoomEvent {
    let Some(confirm) = hex32(confirm_sha256) else {
        return RoomEvent::WaitListFailed {
            request,
            reason: "the confirmation digest must be 64 hex characters".into(),
        };
    };
    match context
        .rooms
        .confirm_wait_list(&context.room, confirm)
        .await
    {
        Ok(confirmed) => RoomEvent::Confirmed { request, confirmed },
        Err(error) => RoomEvent::WaitListFailed {
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
) -> RoomEvent {
    let Some(removal) = hex32(removal_sha256) else {
        return RoomEvent::WaitListFailed {
            request,
            reason: "the removal digest must be 64 hex characters".into(),
        };
    };
    match context
        .rooms
        .wait_list_standing(&context.room, removal)
        .await
    {
        Ok(standing) => RoomEvent::Standing { request, standing },
        Err(error) => RoomEvent::WaitListFailed {
            request,
            reason: error.to_string(),
        },
    }
}

/// The answer to a command about the wait list.
async fn answer_wait_list_command(
    context: &PersistentRoom,
    governance: &Governance,
    command: OperatorCommand,
) -> RoomEvent {
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
            answer_wait_list(context, governance, ask).await
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
        _ => RoomEvent::WaitListFailed {
            request: String::new(),
            reason: "not a wait-list command".into(),
        },
    }
}

/// The event that has the operator mail an invitation a member asked for.
fn invite_mail(mail: tilo_relay::rooms::WaitListMail) -> RoomEvent {
    RoomEvent::InviteMail {
        to: mailbox(mail.to),
        link: mail.link,
        removal: hex::encode(mail.removal),
    }
}

fn answer_spectate(context: &PersistentRoom, device_public_key: String) -> RoomEvent {
    let Some(device) = hex32(&device_public_key) else {
        return RoomEvent::SpectateRefused {
            device_public_key,
            reason: "the device key must be 64 hex characters".into(),
        };
    };
    match spectator_cert(&context.spectator, device, tilo_clock::unix_now_ns()) {
        Ok(certificate) => RoomEvent::SpectatorCertified {
            device_public_key,
            certificate,
        },
        Err(error) => RoomEvent::SpectateRefused {
            device_public_key,
            reason: format!("{error:#}"),
        },
    }
}

/// Make the spectator person a Viewer member, through the same
/// pre-admission and join steps a guest's browser takes, so it is an
/// ordinary member of the room's list.
async fn ensure_spectator_member(
    rooms: &Rooms,
    host_cert: &DeviceCert,
    root: &SigningKey,
    room: &str,
    spectator: &SigningKey,
) -> anyhow::Result<()> {
    let person = spectator.verifying_key().to_bytes();
    let summary = rooms
        .summary(room)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    if summary
        .members
        .iter()
        .any(|member| member.user_public_key == person && !member.removed)
    {
        return Ok(());
    }
    let token: [u8; 32] = rand::random();
    let issued = issue_pre_admission(
        rooms,
        host_cert,
        root,
        room,
        person,
        token_digest(&token),
        SPECTATOR_NAME,
        RoomRole::Viewer,
    )
    .await
    .context("pre-admit the spectator person")?;
    let device = SigningKey::from_bytes(&rand::random());
    let cert = spectator_cert(
        spectator,
        device.verifying_key().to_bytes(),
        tilo_clock::unix_now_ns(),
    )?;
    let challenge = match rooms
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert: cert,
            request_id: rand::random(),
        })
        .await
    {
        JoinReply::Challenge { challenge } => challenge,
        other => bail!("the spectator person's join was answered {other:?}"),
    };
    let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
    match rooms
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: SPECTATOR_NAME.into(),
            token: token.to_vec(),
        })
        .await
    {
        JoinReply::Admitted { .. } => Ok(()),
        other => bail!("the spectator person was not admitted: {other:?}"),
    }
}

async fn apply_role_changes(
    context: &PersistentRoom,
    changes: Vec<RoleChange>,
) -> anyhow::Result<()> {
    for change in changes {
        let now = tilo_clock::unix_now_ns();
        let (key, result) = match change {
            RoleChange::Administrator(key) => (
                key,
                context
                    .rooms
                    .set_member_administrator(
                        &context.room,
                        &context.host_cert,
                        request_id(b"owner", &key, now),
                        key,
                    )
                    .await
                    .map(drop),
            ),
            RoleChange::Moderator(key) => (
                key,
                context
                    .rooms
                    .set_member_role(
                        &context.room,
                        &context.host_cert,
                        SetMemberRoleRequest {
                            request_id: request_id(b"moderator", &key, now),
                            user_public_key: key,
                            role: tilo_peer_state::join::MemberRole::Moderator,
                        },
                    )
                    .await
                    .map(drop),
            ),
            RoleChange::Editor(key) => (
                key,
                context
                    .rooms
                    .set_member_role(
                        &context.room,
                        &context.host_cert,
                        SetMemberRoleRequest {
                            request_id: request_id(b"editor", &key, now),
                            user_public_key: key,
                            role: tilo_peer_state::join::MemberRole::Editor,
                        },
                    )
                    .await
                    .map(drop),
            ),
        };
        result.map_err(|error| anyhow::anyhow!("role of {}: {error}", hex::encode(key)))?;
    }
    Ok(())
}

/// A fresh administration request id per decision: the same person can be
/// promoted, demoted and promoted again over the room's life.
fn request_id(kind: &[u8], key: &[u8; 32], now_ns: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind);
    hasher.update(key);
    hasher.update(&now_ns.to_le_bytes());
    *hasher.finalize().as_bytes()
}

pub(crate) async fn seed_apps(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    apps: &[String],
    seeded_file: &Path,
) -> anyhow::Result<()> {
    let mut seeded: BTreeSet<String> = read_json(seeded_file)?;
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let pending: Vec<&String> = apps.iter().filter(|app| !seeded.contains(*app)).collect();
    for app in pending {
        let record = tilo_relay::room_bots::room_record(&catalogue, app);
        rooms
            .prepare_application(room, cert, &catalogue, app, &record)
            .await
            .map_err(|error| anyhow::anyhow!("seed {app}: {error}"))?;
        seeded.insert(app.clone());
        write_json(seeded_file, &seeded)?;
    }
    Ok(())
}

/// What the room keeps of the files members post in its Chat; `None`
/// when it keeps none.
fn attachment_limits(
    persistent: &PersistentArgs,
) -> Option<tilo_relay::room_bots::attachments::AttachmentLimits> {
    const MIB: u64 = 1024 * 1024;
    (persistent.attachment_room_mib > 0).then(|| {
        tilo_relay::room_bots::attachments::AttachmentLimits {
            file_bytes: persistent.attachment_max_mib.saturating_mul(MIB),
            member_bytes: persistent.attachment_member_mib.saturating_mul(MIB),
            room_bytes: persistent.attachment_room_mib.saturating_mul(MIB),
        }
    })
}

/// [`attachment_limits`] as the room's Chat reads them.
fn room_files(persistent: &PersistentArgs) -> chat::record::RoomFiles {
    attachment_limits(persistent).map_or_else(Default::default, |limits| chat::record::RoomFiles {
        largest: limits.file_bytes,
        member_hourly: limits.member_bytes,
    })
}

/// Give the room's chat, when it is seeded and while it has only the
/// channels every room opens with, the operator's starting `channels`, and
/// tell it what the room keeps of members' `files`. What people wrote is
/// never touched, and a chat with channels keeps exactly those.
pub(crate) async fn settle_chat(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    apps: &[String],
    channels: &[String],
    feedback: Option<&str>,
    files: chat::record::RoomFiles,
    discussions: &[crate::room_discussions::Published],
) -> anyhow::Result<()> {
    if !apps.iter().any(|app| app == crate::room_chat::CHAT_APP) {
        return Ok(());
    }
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let record = tilo_relay::room_bots::room_record(&catalogue, crate::room_chat::CHAT_APP);
    rooms
        .author_application(
            room,
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

/// Give the room's shared desktop the operator's `strips` where missing.
pub(crate) async fn settle_strips(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    shown: &str,
    strips: &[String],
) -> anyhow::Result<()> {
    if strips.is_empty() {
        return Ok(());
    }
    rooms
        .author_room_record(room, cert, crate::room_strips::DESKTOP, |doc| {
            crate::room_strips::settle_strips(doc, room, shown, strips)
        })
        .await
        .map(drop)
        .map_err(|error| anyhow::anyhow!("give the shared desktop its strips: {error}"))
}

/// Make exactly the operator's `apps` authors of the room, so its members
/// write through them, and take authorship back from an app this host made
/// an author at an earlier start and no longer names (kept in `authored`).
pub(crate) async fn settle_app_authorship(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    apps: &[String],
    authored: &Path,
) -> anyhow::Result<()> {
    let earlier: BTreeSet<String> = read_json(authored)?;
    let apps: BTreeSet<String> = apps.iter().cloned().collect();
    let withdrawn: BTreeSet<String> = earlier.difference(&apps).cloned().collect();
    rooms
        .settle_app_authorship(room, cert, &apps, &withdrawn)
        .await
        .map_err(|error| anyhow::anyhow!("let members write through {apps:?}: {error}"))?;
    write_json(authored, &apps)?;
    Ok(())
}

pub(crate) async fn ensure_guest_policy(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
) -> anyhow::Result<()> {
    use tilo_action_policy::{Answer, Grant, KindSet, Role, Subject, TreePath};
    // Guests are Editors; in a public room they must not trigger outside
    // effects from any app (sending mail, calling services).
    let deny = Grant::new(
        "public-room-guests-no-outside",
        Subject::Role(Role::Editor),
        TreePath::room(room),
        KindSet::OUTSIDE,
        Answer::Deny,
        Subject::Person(hex::encode(cert.user_public_key)),
    );
    let digest = *blake3::hash(b"public-room-guests-no-outside/v1").as_bytes();
    rooms
        .put_room_grants(room, cert, digest, vec![deny])
        .await
        .map_err(|error| anyhow::anyhow!("deny outside effects to guests: {error}"))?;
    Ok(())
}

/// The grant letting the room's members (guests are Editors) invite
/// someone, which a room taking people by invitation holds and an open one
/// does not: anyone joins an open room through its door.
const MEMBERS_INVITE: &str = "members-invite";

/// Let the room's members invite people, and let in whom they invited,
/// while the room takes people by invitation (`members_only`); take that
/// back once it lets anyone in again. Settled at every start.
pub(crate) async fn settle_member_invites(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    members_only: bool,
) -> anyhow::Result<()> {
    use tilo_action_policy::{Answer, Grant, GrantId, KindSet, Role, Subject, roles};
    // One request per start and setting: the same setting settled again
    // after the room changed writes it again, which changes nothing.
    let generation = rooms
        .summary(room)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .generation;
    let request_id =
        *blake3::hash(format!("{MEMBERS_INVITE}/{members_only}/{generation}").as_bytes())
            .as_bytes();
    if members_only {
        let invite = Grant::new(
            MEMBERS_INVITE,
            Subject::Role(Role::Editor),
            roles::governance_action(room, roles::governance::INVITE),
            KindSet::OUTSIDE,
            Answer::Allow,
            Subject::Person(hex::encode(cert.user_public_key)),
        );
        rooms
            .put_room_grants(room, cert, request_id, vec![invite])
            .await
            .map_err(|error| anyhow::anyhow!("let members invite: {error}"))?;
    } else {
        rooms
            .remove_room_grants(room, cert, request_id, vec![GrantId(MEMBERS_INVITE.into())])
            .await
            .map_err(|error| anyhow::anyhow!("take back members inviting: {error}"))?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one startup sequence; the hosted relay lives until shutdown"
)]
pub(crate) async fn run(
    args: RoomArgs,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let persistent = args.persistent.clone();
    let name = persistent
        .persistent
        .clone()
        .context("--persistent names the room")?;
    let root = read_root_file(persistent.root_file.as_deref().context("--root-file")?)?;
    let id_file = persistent.room_id_file.clone().context("--room-id-file")?;
    let owners = parse_keys("owner", &persistent.owners)?;
    let moderators = parse_keys("moderator", &persistent.moderators)?;
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
    let listen = args.web.web.context("a room needs --web")?;
    let mut operator = operator_lines()?;
    let store = tilo_secrets::SecretStore::for_app(SECRET_APP);
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
    // The same model looks over the reports the room's members write
    // before they send them, asked through the room on their behalf.
    let feedback_reviewer = moderation
        .is_some()
        .then(|| {
            crate::persistent_moderation::RoomReviewer::from_store(
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
        &format!("{name} room host"),
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    )?;
    let config = tilo_relay::RelayConfig {
        state_root: tilo_relay_protocol::metrics::room_relay_root(&state),
        identity_root: state.join("identity"),
        member_limits: persistent.member_limits.then(Default::default),
        ..Default::default()
    };
    let hosted = start_relay_lending(
        config,
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host,
            device_cert: host_cert.clone(),
            revocations: RevocationList::new(),
        },
        &args.web,
        &state,
        crate::web_cli::Lent { feedback_reviewer },
    )
    .await?;
    let rooms = hosted
        .relay
        .rooms()
        .context("the relay governs no rooms in this mode")?
        .clone();
    let shown = persistent
        .workspace_name
        .clone()
        .unwrap_or_else(|| workspace_name(&name));
    let room = governed_room(&rooms, &shown, &id_file, &store, &root).await?;
    // Every visitor comes in through the site's door: unless the host says
    // otherwise, the room shares its apps' records, never what anyone has
    // open. Settled at every start, as the host configured it.
    rooms
        .set_layout(&room, persistent.layout)
        .await
        .map_err(|error| anyhow::anyhow!("settle the room's layout scope: {error}"))?;
    let spectator = spectator_root(&store, &room)?;
    ensure_spectator_member(&rooms, &host_cert, &root, &room, &spectator).await?;
    let workspace_id = rooms
        .summary(&room)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .workspace_id;
    hosted
        .relay
        .admit_spectators(
            workspace_id,
            tilo_relay::Spectators {
                root: spectator.verifying_key().to_bytes(),
                seats: persistent.max_spectators,
                // Every visitor may join this room as a guest, and so read
                // its channels: one who only watches may read them too.
                public: true,
            },
        )
        .await
        .context("let visitors watch the room")?;
    seed_apps(
        &rooms,
        &room,
        &host_cert,
        &persistent.seed_apps,
        &state.join("seeded.json"),
    )
    .await?;
    settle_chat(
        &rooms,
        &room,
        &host_cert,
        &persistent.seed_apps,
        &persistent.chat_channels,
        persistent.feedback_channel.as_deref(),
        room_files(&persistent),
        &persistent.discussions,
    )
    .await?;
    settle_strips(&rooms, &room, &host_cert, &shown, &persistent.strips).await?;
    ensure_guest_policy(&rooms, &room, &host_cert).await?;
    settle_member_invites(&rooms, &room, &host_cert, persistent.members_only).await?;
    settle_app_authorship(
        &rooms,
        &room,
        &host_cert,
        &persistent.author_apps,
        &state.join("authored-apps.json"),
    )
    .await?;
    let web = hosted
        .relay
        .serve_web(web_config(&args.web, listen, &state))
        .await
        .context("start the browser listener")?;
    println!(
        "{}",
        RoomEvent::Started {
            version: room_operator::VERSION,
            origin: web.origin.clone(),
            plain_origin: web.plain_origin.clone(),
            user_public_key: hex::encode(root.verifying_key().to_bytes()),
            certificate_sha256: web.certificate_sha256.clone(),
            allowed_origins: args.web.allowed_origins.clone(),
            room: Some(room.clone()),
        }
        .to_line()
    );
    // The moderation agent runs as the room's bot for as long as the room
    // does; it answers reports left open while the room was down first.
    let moderator = moderation.map(|agent| {
        tokio::spawn(
            tilo_relay::room_bots::ModerationBot::new(
                rooms.clone(),
                room.clone(),
                host_cert.clone(),
                agent,
            )
            .run(),
        )
    });
    // The connectors bots listen to, the operator's and those a member
    // sets up for a bot they ask for, share one host and one blob store.
    let public = match (&persistent.public_avatars, &persistent.public_avatars_url) {
        (Some(dir), Some(url)) => Some(
            tilo_relay::public_avatars::PublicAvatars::new(dir.clone(), url)
                .with_context(|| format!("prepare {}", dir.display()))?,
        ),
        _ => None,
    };
    let connector_host = tilo_relay::room_bots::installed::ConnectorHost::new(
        workers,
        tilo_relay::room_bots::installed::FsBlobs::new(state.join("blobs"), public)
            .context("prepare the blob store")?,
        tilo_relay::room_bots::discord::Served {
            avatars_url: persistent.public_avatars_url.clone(),
            web_entry: args.web.web_entry.clone(),
            origin: Some(web.origin.clone()),
        },
    );
    let runners = start_bots(
        &rooms,
        &room,
        &host_cert,
        &root,
        &bots,
        &connector_host,
        &persistent,
        &state,
    )
    .await?;
    // The files members post in the room's Chat, kept in the same store
    // as what the bots fetch, so they stay once their poster left.
    let attachments = attachment_limits(&persistent).map(|limits| {
        tokio::spawn(
            tilo_relay::room_bots::attachments::RoomAttachments::new(
                rooms.clone(),
                room.clone(),
                host_cert.device_public_key,
                connector_host.store(&rooms),
                state.join("attachments.json"),
                limits,
            )
            .run(),
        )
    });
    // Bots the room's members ask for, beside the operator's.
    let requested =
        tilo_relay::room_bots::requested::spawn(tilo_relay::room_bots::requested::Follower {
            rooms: rooms.clone(),
            room: room.clone(),
            host: host_cert.clone(),
            root: root.clone(),
            state: state.clone(),
            operator: bots.iter().map(|spec| spec.entry.clone()).collect(),
            connectors: Some(connector_host),
        });
    let context = PersistentRoom {
        rooms,
        room,
        root,
        spectator,
        host_cert,
        web,
    };
    let governance = Governance {
        owners,
        moderators,
        roles_file: persistent.roles_file.clone(),
        inactive_after_mins: persistent.inactive_after_days.saturating_mul(24 * 60),
        state: state.clone(),
        members_only: persistent.members_only,
        wait_list_max: persistent.wait_list_max,
        wait_list_mail: persistent.wait_list_mail.clone(),
    };
    let result = tokio::select! {
        result = serve_operator(&context, &governance, &mut operator) => result,
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

/// Connect the room to the Discord server a bridge's inputs name
/// ([`discord::link_bridge`](tilo_relay::room_bots::discord::link_bridge)),
/// its posts pointing at what `connectors`' host serves publicly.
async fn link_discord(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    spec: &BotSpec,
    bot: &mut tilo_relay::room_bots::installed::InstalledBot,
    connectors: &tilo_relay::room_bots::installed::ConnectorHost,
) -> Result<Vec<(String, String)>, String> {
    let public = &connectors.public;
    tilo_relay::room_bots::discord::link_bridge(rooms, room, host_cert, &spec.inputs, bot, public)
        .await
}

/// Install the bots the operator named (once: a restart finds the same
/// installations), start the connectors their programs listen to, and run
/// each bot on this host. Each is kept a governed member of the room, its
/// delegation renewed with the room's root as it runs out.
async fn start_bots(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    root: &SigningKey,
    bots: &[BotSpec],
    connector_host: &tilo_relay::room_bots::installed::ConnectorHost,
    persistent: &PersistentArgs,
    state: &Path,
) -> anyhow::Result<Vec<tokio::task::JoinHandle<()>>> {
    use tilo_relay::room_bots::discord;
    use tilo_relay::room_bots::installed::{self, BotRunner, InstalledBot};
    if let Some(path) = &persistent.discord_token_file {
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("read the Discord token file {}", path.display()))?;
        // Never logged: only kept where the connector's worker asks for it.
        installed::put_connector_secret(
            discord::APP,
            discord::TOKEN_PURPOSE,
            token.trim().as_bytes(),
        )
        .map_err(|error| anyhow::anyhow!("keep the Discord token: {error}"))?;
    }
    let mut installed_bots = Vec::new();
    let mut connectors = BTreeSet::new();
    let mut settings = BTreeMap::new();
    for spec in bots {
        let inputs = spec
            .inputs
            .iter()
            .map(|(key, value)| (key.clone(), input_value(value)))
            .collect();
        let dir = state.join("bots").join(slug_of(&spec.entry));
        let mut bot = installed::install(&dir, &spec.entry, inputs)
            .await
            .map_err(|error| anyhow::anyhow!("install {}: {error}", spec.entry))?;
        let shortcut = bot.shortcut().map_err(|error| anyhow::anyhow!("{error}"))?;
        let wanted = InstalledBot::connectors(&shortcut);
        if wanted.contains(discord::APP) {
            match link_discord(rooms, room, host_cert, spec, &mut bot, connector_host).await {
                Ok(env) => {
                    settings.insert(discord::APP.to_owned(), env);
                }
                Err(error) => {
                    tracing::error!(entry = %spec.entry, %error, "not running the Discord bridge");
                    continue;
                }
            }
        }
        connectors.extend(wanted);
        tracing::info!(entry = %spec.entry, installation = %bot.installation, "bot installed");
        installed_bots.push((dir, bot));
    }
    if installed_bots.is_empty() {
        return Ok(Vec::new());
    }
    let blobs = connector_host.blobs(rooms);
    // The room serves without its bots rather than not at all: a connector
    // whose worker does not start (a missing binary, an unreachable service)
    // leaves the bots that need it out, said in the log, as a bridge that
    // does not link does.
    let started = match installed::start_connectors(
        &connectors,
        state,
        &connector_host.workers,
        &settings,
        &blobs,
    )
    .await
    {
        Ok(started) => started,
        Err(error) => {
            tracing::error!(%error, "not running the room's bots: a connector did not start");
            return Ok(Vec::new());
        }
    };
    let mut runners = Vec::new();
    for (dir, bot) in installed_bots {
        let wanted =
            InstalledBot::connectors(&bot.shortcut().map_err(|error| anyhow::anyhow!("{error}"))?);
        let own = started
            .iter()
            .filter(|(app, _)| wanted.contains(*app))
            .map(|(app, process)| (app.clone(), Arc::clone(process)))
            .collect::<BTreeMap<_, _>>();
        let certifier = tilo_relay::room_bots::membership::HostRootCertifier::new(root.clone());
        if let Some(lane) = own.get(discord::APP).cloned() {
            runners.extend(carry_calls(rooms, room, host_cert, &certifier, &bot, lane));
        }
        let runner = BotRunner::new(
            rooms.clone(),
            room.to_owned(),
            host_cert.clone(),
            certifier.device_key(&bot.installation),
            bot.clone(),
            own,
            dir.join("progress.json"),
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        runners.push(tokio::spawn(
            tilo_relay::room_bots::membership::keep_renewed(
                rooms.clone(),
                room.to_owned(),
                host_cert.clone(),
                certifier,
                bot.clone(),
            ),
        ));
        runners.push(tokio::spawn(runner.run()));
    }
    Ok(runners)
}

/// Carry `room`'s calls to Discord and back as `bot`'s device, through the
/// Discord connector's worker `lane` the bot's runner uses too.
fn carry_calls(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    certifier: &tilo_relay::room_bots::membership::HostRootCertifier,
    bot: &tilo_relay::room_bots::installed::InstalledBot,
    lane: Arc<impl tilo_relay::room_bots::discord_voice::CallLane>,
) -> Option<tokio::task::JoinHandle<()>> {
    match certifier.device(room, bot, tilo_clock::unix_now_ns()) {
        Ok(device) => Some(tokio::spawn(
            tilo_relay::room_bots::discord_voice::VoiceBridge::new(
                rooms.clone(),
                room.to_owned(),
                host_cert.clone(),
                device.cert,
                lane,
            )
            .run(),
        )),
        Err(error) => {
            tracing::error!(entry = %bot.entry, %error, "not carrying the Discord bridge's calls");
            None
        }
    }
}

/// A file name for directory entry or program path `entry`.
fn slug_of(entry: &str) -> String {
    entry
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The operator's declarations the room is kept in line with.
struct Governance {
    owners: BTreeSet<[u8; 32]>,
    moderators: BTreeSet<[u8; 32]>,
    /// More owners and moderators, read again whenever the file changes.
    roles_file: Option<PathBuf>,
    inactive_after_mins: u64,
    state: PathBuf,
    /// The room takes people by invitation (`--members-only`).
    members_only: bool,
    /// Requests left at the door it keeps (`--wait-list-max`).
    wait_list_max: usize,
    /// The door whoever runs it sends mail from (`--wait-list-mail`).
    wait_list_mail: Option<String>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct DeclaredRoles {
    owners: Vec<String>,
    moderators: Vec<String>,
}

impl Governance {
    fn declared_file(&self) -> PathBuf {
        self.state.join("declared-roles.json")
    }

    fn removals_file(&self) -> PathBuf {
        self.state.join("removals.json")
    }

    fn last_seen_file(&self) -> PathBuf {
        self.state.join("last-seen.json")
    }

    fn previously_declared(&self) -> anyhow::Result<BTreeSet<[u8; 32]>> {
        let declared: DeclaredRoles = read_json(&self.declared_file())?;
        Ok(declared
            .owners
            .iter()
            .chain(&declared.moderators)
            .filter_map(|key| hex32(key))
            .collect())
    }

    /// The owners and moderators declared now: the command line's and the
    /// roles file's. A file that cannot be read declares nothing more.
    fn current(&self) -> (BTreeSet<[u8; 32]>, BTreeSet<[u8; 32]>) {
        let mut owners = self.owners.clone();
        let mut moderators = self.moderators.clone();
        if let Some(path) = &self.roles_file {
            match read_json::<DeclaredRoles>(path) {
                Ok(file) => {
                    owners.extend(file.owners.iter().filter_map(|key| hex32(key)));
                    moderators.extend(file.moderators.iter().filter_map(|key| hex32(key)));
                }
                Err(error) => eprintln!("persistent room: roles file unreadable: {error}"),
            }
        }
        (owners, moderators)
    }

    /// When the roles file last changed, to take it in again.
    fn roles_changed_at(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.roles_file.as_ref()?)
            .ok()?
            .modified()
            .ok()
    }

    fn remember_declared(
        &self,
        (owners, moderators): &(BTreeSet<[u8; 32]>, BTreeSet<[u8; 32]>),
    ) -> anyhow::Result<()> {
        write_json(
            &self.declared_file(),
            &DeclaredRoles {
                owners: owners.iter().map(hex::encode).collect(),
                moderators: moderators.iter().map(hex::encode).collect(),
            },
        )
    }

    fn read_last_seen(&self) -> anyhow::Result<BTreeMap<[u8; 32], u64>> {
        let stored: BTreeMap<String, u64> = read_json(&self.last_seen_file())?;
        Ok(stored
            .into_iter()
            .filter_map(|(key, seen)| Some((hex32(&key)?, seen)))
            .collect())
    }

    fn write_last_seen(&self, last_seen: &BTreeMap<[u8; 32], u64>) -> anyhow::Result<()> {
        let stored: BTreeMap<String, u64> = last_seen
            .iter()
            .map(|(key, seen)| (hex::encode(key), *seen))
            .collect();
        write_json(&self.last_seen_file(), &stored)
    }
}

async fn summary(
    context: &PersistentRoom,
) -> anyhow::Result<tilo_relay_session::rooms::RoomSummary> {
    context
        .rooms
        .summary(&context.room)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}

async fn reconcile_roles(
    context: &PersistentRoom,
    governance: &Governance,
    previously_declared: &mut BTreeSet<[u8; 32]>,
) -> anyhow::Result<()> {
    let members = summary(context).await?.members;
    let current = governance.current();
    let changes = role_changes(&members, &current.0, &current.1, previously_declared);
    apply_role_changes(context, changes).await?;
    governance.remember_declared(&current)?;
    *previously_declared = current.0.union(&current.1).copied().collect();
    Ok(())
}

/// Removes guests idle past the horizon, in batches. A batch the room
/// refuses stays seated until the next call.
async fn prune_idle(
    context: &PersistentRoom,
    governance: &Governance,
    spectator: [u8; 32],
    removals: &Removals,
    last_seen: &mut BTreeMap<[u8; 32], u64>,
) -> anyhow::Result<()> {
    let members = summary(context).await?.members;
    let now = tilo_clock::unix_now_ms();
    for member in members.iter().filter(|member| !member.removed) {
        last_seen.entry(member.user_public_key).or_insert(now);
    }
    let (owners, moderators) = governance.current();
    let mut keep: BTreeSet<[u8; 32]> = owners.union(&moderators).copied().collect();
    keep.insert(spectator);
    let idle = idle_guests(
        &members,
        last_seen,
        &keep,
        now,
        governance.inactive_after_mins,
    );
    for batch in idle.chunks(PRUNE_BATCH) {
        let batch: BTreeSet<[u8; 32]> = batch.iter().copied().collect();
        // Into `removed` first: a pruned guest is not a removal the service
        // should cool their address down for.
        removals.pruning(&batch);
        let first = batch.first().copied().unwrap_or_default();
        if let Err(error) = context
            .rooms
            .prune_members(
                &context.room,
                &context.host_cert,
                request_id(b"prune", &first, tilo_clock::unix_now_ns()),
                &batch,
            )
            .await
        {
            // They keep their seats until the next tick tries again, and
            // the room goes on serving: ending here dropped every visitor's
            // session and turned each new one away while the room service
            // restarted it.
            removals.not_pruned(&batch);
            tracing::warn!("pruning {} idle guests failed: {error}", batch.len());
            break;
        }
        for key in &batch {
            last_seen.remove(key);
        }
    }
    // Addresses left at the door and not confirmed in time go too, also
    // while nobody touches the wait list.
    if governance.wait_list_max > 0
        && let Err(error) = context.rooms.expire_wait_list(&context.room).await
    {
        tracing::warn!("the wait list kept unconfirmed addresses: {error}");
    }
    governance.write_last_seen(last_seen)
}

/// How often the operator's roles file is looked at for a change.
const ROLES_FILE_EVERY_SECS: u64 = 2;

/// What the room last said about who is online, and when it last wrote
/// down when each member was last seen.
struct OnlineWatch {
    last_online: Option<(u32, u32, Vec<tilo_relay_protocol::directory::Face>, u32)>,
    last_seen_written: tokio::time::Instant,
}

impl OnlineWatch {
    /// Say who is online when it changed, note everyone online as seen now,
    /// and write the last-seen times down at most once a minute.
    async fn observe(
        &mut self,
        context: &PersistentRoom,
        governance: &Governance,
        spectator: [u8; 32],
        last_seen: &mut BTreeMap<[u8; 32], u64>,
    ) -> anyhow::Result<()> {
        let sessions = context.web.online_users();
        let people = sessions.keys().filter(|user| **user != spectator).count();
        // Seated ones only: a page refused for want of a seat keeps
        // its web session open while it waits, but watches nothing.
        let spectators = context
            .rooms
            .seated_spectators(&context.room)
            .await
            .unwrap_or(0);
        // Who of the members is here, for the site's directory.
        let here: std::collections::BTreeSet<[u8; 32]> = sessions
            .keys()
            .filter(|user| **user != spectator)
            .copied()
            .collect();
        let (faces, _, members) = context
            .rooms
            .presence(&context.room, &here)
            .await
            .unwrap_or_default();
        let online = (
            u32::try_from(people).unwrap_or(u32::MAX),
            u32::try_from(spectators).unwrap_or(u32::MAX),
            faces,
            // The spectator person is a member nobody counts.
            members.saturating_sub(1),
        );
        if self.last_online.as_ref() != Some(&online) {
            println!(
                "{}",
                RoomEvent::Online {
                    people: online.0,
                    spectators: online.1,
                    here: online.2.clone(),
                    members: online.3,
                }
                .to_line()
            );
            self.last_online = Some(online);
        }
        let now = tilo_clock::unix_now_ms();
        for user in sessions.keys() {
            last_seen.insert(*user, now);
        }
        if self.last_seen_written.elapsed() >= Duration::from_secs(60) {
            for member in summary(context)
                .await?
                .members
                .iter()
                .filter(|member| !member.removed)
            {
                last_seen.entry(member.user_public_key).or_insert(now);
            }
            governance.write_last_seen(last_seen)?;
            self.last_seen_written = tokio::time::Instant::now();
        }
        Ok(())
    }
}

async fn serve_operator(
    context: &PersistentRoom,
    governance: &Governance,
    operator: &mut tokio::sync::mpsc::Receiver<Result<OperatorCommand, String>>,
) -> anyhow::Result<()> {
    let mut changes = context.rooms.governance_changes();
    let spectator = context.spectator.verifying_key().to_bytes();
    let removals = Removals::load(
        governance.removals_file(),
        &summary(context).await?.members,
        tilo_clock::unix_now_ms(),
    )?;
    let mut previously_declared = governance.previously_declared()?;
    reconcile_roles(context, governance, &mut previously_declared).await?;
    let mut last_seen = governance.read_last_seen()?;
    let mut presence = OnlineWatch {
        last_online: None,
        last_seen_written: tokio::time::Instant::now(),
    };
    let mut online_tick =
        tokio::time::interval(Duration::from_secs(room_operator::ONLINE_EVERY_SECS));
    // A quarter of the horizon, so nobody stays much past it, and not more
    // often than a minute.
    let mut prune_tick = tokio::time::interval(Duration::from_secs(
        governance
            .inactive_after_mins
            .saturating_mul(15)
            .clamp(60, 3600),
    ));
    // The operator's roles file, looked at every few seconds: a role it
    // grants or takes back reaches the room without a restart.
    let mut roles_tick = tokio::time::interval(Duration::from_secs(ROLES_FILE_EVERY_SECS));
    let mut roles_seen = governance.roles_changed_at();
    let admitter = Admitter::of(context, removals.clone(), governance.members_only);
    let mut admitting = tokio::task::JoinSet::<RoomEvent>::new();
    let mut mail = governance
        .wait_list_mail
        .as_ref()
        .and_then(|door| context.rooms.mail_wait_list(door));
    loop {
        tokio::select! {
            Some(asked) = async {
                match mail.as_mut() {
                    Some(mail) => mail.recv().await,
                    None => std::future::pending().await,
                }
            } => println!("{}", invite_mail(asked).to_line()),
            Some(answered) = admitting.join_next(), if !admitting.is_empty() => match answered {
                Ok(event) => println!("{}", event.to_line()),
                // Its waiter hears nothing and gives up, as on any lost answer.
                Err(error) => eprintln!("tilo-relay: a pre-admission did not finish: {error}"),
            },
            _ = roles_tick.tick(), if governance.roles_file.is_some() => {
                let changed_at = governance.roles_changed_at();
                if changed_at != roles_seen {
                    roles_seen = changed_at;
                    reconcile_roles(context, governance, &mut previously_declared).await?;
                }
            }
            _ = online_tick.tick() => {
                presence.observe(context, governance, spectator, &mut last_seen).await?;
            }
            _ = prune_tick.tick() => prune_idle(context, governance, spectator, &removals, &mut last_seen).await?,
            changed = changes.recv() => {
                match changed {
                    Ok(room) if room != context.room => continue,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => bail!("the relay stopped governing"),
                }
                let members = summary(context).await?.members;
                for user in removals.taken_in(&members, tilo_clock::unix_now_ms())? {
                    // Bans exist once room moderation lands; until then every
                    // removal is a plain removal. The service treats both alike.
                    println!("{}", RoomEvent::MemberRemoved { user_public_key: hex::encode(user), banned: false }.to_line());
                }
                reconcile_roles(context, governance, &mut previously_declared).await?;
            }
            line = operator.recv(), if admitting.len() < PRE_ADMITTING_AT_ONCE => match line {
                Some(Ok(OperatorCommand::PreAdmit { user_public_key, token_sha256, label, role })) => {
                    admitting.spawn(answer_pre_admit(admitter.clone(), user_public_key, token_sha256, label, role));
                }
                Some(Ok(OperatorCommand::Spectate { device_public_key })) => {
                    println!("{}", answer_spectate(context, device_public_key).to_line());
                }
                Some(Ok(command @ (OperatorCommand::WaitList { .. }
                | OperatorCommand::Unlist { .. }
                | OperatorCommand::Confirm { .. }
                | OperatorCommand::Standing { .. }))) => {
                    println!("{}", answer_wait_list_command(context, governance, command).await.to_line());
                }
                Some(Ok(OperatorCommand::Admit { .. })) => bail!("a persistent room admits guests by pre-admission only"),
                Some(Ok(OperatorCommand::Shutdown {})) | None => {
                    // Whoever is being let in hears the answer before the room stops.
                    while let Some(answered) = admitting.join_next().await {
                        if let Ok(event) = answered {
                            println!("{}", event.to_line());
                        }
                    }
                    return Ok(());
                }
                Some(Err(error)) => bail!("unreadable room operator line: {error}"),
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
    use tilo_relay_protocol::rooms::{RoomsReply, RoomsRequest};
    const DAY_MS: u64 = 24 * 3600 * 1000;

    #[test]
    fn the_workspace_is_named_after_the_room_capitalized() {
        assert_eq!(workspace_name("plaza"), "Plaza");
        assert_eq!(workspace_name(""), "");
    }

    fn store() -> tilo_secrets::SecretStore {
        tilo_secrets::SecretStore::memory(SECRET_APP)
    }

    fn member(key: u8, administrator: bool) -> tilo_peer_state::GovernedMember {
        tilo_peer_state::GovernedMember {
            user_public_key: [key; 32],
            role: tilo_peer_state::join::MemberRole::Editor,
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
        check_stored_root(&store, "room-a", &root).expect_err("no stored root is refused");
        keep_root(&store, "room-a", &root).unwrap();
        check_stored_root(&store, "room-a", &root).unwrap();
        let refused = check_stored_root(&store, "room-a", &other).unwrap_err();
        assert!(
            refused.to_string().contains("not this room's owner"),
            "{refused:#}"
        );
    }

    #[test]
    fn the_spectator_root_is_made_once_and_kept() {
        let store = store();
        let first = spectator_root(&store, "room-a").unwrap();
        assert_eq!(
            spectator_root(&store, "room-a").unwrap().to_bytes(),
            first.to_bytes()
        );
        assert_ne!(
            spectator_root(&store, "room-b").unwrap().to_bytes(),
            first.to_bytes()
        );
    }

    #[test]
    fn a_spectator_certificate_lasts_an_hour_and_carries_no_capabilities() {
        let spectator = SigningKey::from_bytes(&[0x33; 32]);
        let cert = spectator_cert(&spectator, [4; 32], 1_000).unwrap();
        tilo_user_identity::verify_device_cert_at(&cert, 1_000).unwrap();
        assert_eq!(cert.user_public_key, spectator.verifying_key().to_bytes());
        assert_eq!(cert.device_public_key, [4; 32]);
        assert_eq!(
            cert.expires_at_ns,
            Some(1_000 + SPECTATOR_CERT_SECS * NS_PER_SEC)
        );
        let caps = &cert.capabilities;
        assert!(
            !(caps.can_own_workspaces
                || caps.can_admit_devices
                || caps.can_act_as_host
                || caps.can_read_all_workspaces
                || caps.can_administer_devices)
        );
        assert!(
            tilo_user_identity::verify_device_cert_at(
                &cert,
                1_000 + SPECTATOR_CERT_SECS * NS_PER_SEC + 1
            )
            .is_err()
        );
    }

    #[test]
    fn a_pre_admission_is_signed_by_the_room_root_for_ten_minutes() {
        let root = SigningKey::from_bytes(&[0x11; 32]);
        let grant =
            pre_admission(&root, "room-a", [5; 32], "Ada", RoomRole::Editor, 1_000).unwrap();
        grant.verify().unwrap();
        assert_eq!(grant.owner_user_public_key, root.verifying_key().to_bytes());
        assert_eq!(grant.expires_at_ns, 1_000 + PRE_ADMISSION_SECS * NS_PER_SEC);
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

    /// A guest pruned after an absence comes back through the door as
    /// anyone does; only moderation's removals keep a person out.
    #[test]
    fn a_pruned_guest_is_not_kept_out_and_a_removal_lapses_after_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("removals.json");
        let gone = |key: u8| tilo_peer_state::GovernedMember {
            removed: true,
            ..member(key, false)
        };
        let removals = Removals::load(file.clone(), &[], 1_000).unwrap();
        removals.pruning(&BTreeSet::from([[5; 32]]));
        assert!(!removals.refuses([5; 32], &[gone(5)], 1_000));
        assert!(removals.taken_in(&[gone(5)], 1_000).unwrap().is_empty());
        assert!(
            removals.refuses([6; 32], &[gone(6)], 1_000),
            "removed, not yet taken in"
        );
        assert_eq!(removals.taken_in(&[gone(6)], 1_000).unwrap(), vec![[6; 32]]);
        let members = [gone(5), gone(6)];
        // Kept across a restart, for the day and no longer.
        let reloaded = Removals::load(file, &members, 2_000).unwrap();
        assert!(reloaded.refuses([6; 32], &members, 2_000));
        assert!(!reloaded.refuses([6; 32], &members, 1_000 + REMOVED_FOR_MS));
        assert!(!reloaded.refuses([5; 32], &members, 2_000));
    }

    /// A relay governing one room `Plaza`, and what answers its
    /// pre-admissions, removals kept in `dir`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the rooms handle moves into what answers pre-admissions"
    )]
    async fn plaza(dir: &Path) -> (tilo_relay::daemon::Relay, Admitter) {
        let root = SigningKey::from_bytes(&[1; 32]);
        let host = SigningKey::from_bytes(&[2; 32]);
        let host_cert = enrol(&root, &host, "host", DeviceCapabilities::host()).unwrap();
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
        let rooms = relay.rooms().unwrap().clone();
        let room = rooms.create("Plaza").await.unwrap().room;
        let removals =
            Removals::load(dir.join("removals.json"), &[], tilo_clock::unix_now_ms()).unwrap();
        let admitter = Admitter {
            rooms,
            host_cert,
            root,
            room,
            spectator: [9; 32],
            removals,
            members_only: false,
        };
        (relay, admitter)
    }

    /// Whether the guest `device` joins with the pre-admission `answer`
    /// and its `token`, as their page does.
    async fn joins(
        rooms: &Rooms,
        (device, device_cert): &(SigningKey, DeviceCert),
        answer: RoomEvent,
        token: &[u8],
    ) -> bool {
        let RoomEvent::PreAdmitted { invitation, .. } = answer else {
            panic!("not pre-admitted: {answer:?}");
        };
        let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(invitation)
            .unwrap();
        let JoinReply::Challenge { challenge } = rooms
            .page_join(JoinStep::Ask {
                invitation,
                device_cert: device_cert.clone(),
                request_id: rand::random(),
            })
            .await
        else {
            return false;
        };
        let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
        let proved = rooms
            .page_join(JoinStep::ProvePreAdmitted {
                challenge,
                signature,
                name: "Guest".into(),
                token: token.to_vec(),
            })
            .await;
        matches!(proved, JoinReply::Admitted { .. })
    }

    fn listed_removed(members: &[tilo_peer_state::GovernedMember], person: [u8; 32]) -> bool {
        members
            .iter()
            .any(|member| member.user_public_key == person && member.removed)
    }

    /// A guest moderation removed asks the door again at once, as their
    /// page does the moment the room drops it: the room refuses that
    /// person, whether or not its operator loop has taken the removal in
    /// yet, and an invitation it issued them before the removal is no way
    /// back in either.
    #[tokio::test]
    async fn a_removed_guest_is_refused_pre_admission_and_kept_out() {
        let dir = tempfile::tempdir().unwrap();
        let (relay, admitter) = plaza(dir.path()).await;
        let (rooms, room) = (admitter.rooms.clone(), admitter.room.clone());
        let device = SigningKey::from_bytes(&[4; 32]);
        let guest_root = SigningKey::from_bytes(&[3; 32]);
        let guest = (
            device.clone(),
            enrol(&guest_root, &device, "guest", DeviceCapabilities::member()).unwrap(),
        );
        let person = guest.1.user_public_key;
        let pre_admit = |token: &'static [u8]| {
            answer_pre_admit(
                admitter.clone(),
                hex::encode(person),
                hex::encode(token_digest(token)),
                "Guest".into(),
                RoomRole::Editor,
            )
        };
        let removed =
            |event: &RoomEvent| matches!(event, RoomEvent::PreAdmitRefused { removed: true, .. });

        assert!(joins(&rooms, &guest, pre_admit(b"first").await, b"first").await);
        let earlier = pre_admit(b"earlier").await;
        let removal = tilo_relay_session::rooms::RemoveMemberRequest {
            request_id: [8; 32],
            user_public_key: person,
        };
        rooms
            .remove_member(&room, &admitter.host_cert, removal)
            .await
            .unwrap();
        let members = rooms.summary(&room).await.unwrap().members;
        assert!(
            listed_removed(&members, person),
            "the removal takes their membership"
        );
        // Before the operator loop has heard of the removal.
        let asked = pre_admit(b"second").await;
        assert!(removed(&asked), "{asked:?}");
        // Taken in: reported once, and still refused.
        let now = tilo_clock::unix_now_ms();
        let taken = admitter.removals.taken_in(&members, now).unwrap();
        assert_eq!(taken, vec![person]);
        let asked = pre_admit(b"third").await;
        assert!(removed(&asked), "{asked:?}");
        assert!(
            !joins(&rooms, &guest, earlier, b"earlier").await,
            "an earlier invitation lets them back in"
        );
        let members = rooms.summary(&room).await.unwrap().members;
        assert!(listed_removed(&members, person), "still removed");
        relay.shutdown().await;
    }

    /// Ask for an invitation to `room` as `device`, as a member's page does.
    async fn invite(rooms: &Rooms, room: &str, device: &DeviceCert) -> RoomsReply {
        rooms
            .page_rooms(
                device,
                RoomsRequest::Invite {
                    room: room.to_owned(),
                    request_id: rand::random(),
                    role: RoomRole::Editor,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3600 * NS_PER_SEC),
                    inviter_name: Some("Member".into()),
                    wait_list_entry: None,
                },
            )
            .await
    }

    /// A room taking people by invitation lets nobody new in through its
    /// door, and still lets a member in again: another tab of theirs is
    /// another device of the same person. A member invites someone with an
    /// invitation of their own and lets them in; once the room lets anyone
    /// in again, members no longer invite.
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one story: the door, a member's other tab, an invitation and taking it back"
    )]
    #[cfg_attr(
        feature = "agents",
        expect(
            clippy::significant_drop_tightening,
            reason = "Relay::shutdown consumes the relay, and so its rooms, after the assertions"
        )
    )]
    async fn taking_people_by_invitation_members_invite_whom_the_door_turns_away() {
        let dir = tempfile::tempdir().unwrap();
        let (relay, open) = plaza(dir.path()).await;
        let rooms = open.rooms.clone();
        let room = open.room.clone();
        ensure_guest_policy(&rooms, &room, &open.host_cert)
            .await
            .unwrap();
        // An open room settles as one at every start, holding no such grant.
        settle_member_invites(&rooms, &room, &open.host_cert, false)
            .await
            .unwrap();
        let by_invitation = Admitter {
            members_only: true,
            ..open.clone()
        };
        let guest = |seed: u8| {
            let device = SigningKey::from_bytes(&[seed; 32]);
            let root = SigningKey::from_bytes(&[seed.wrapping_add(100); 32]);
            let cert = enrol(&root, &device, "guest", DeviceCapabilities::member()).unwrap();
            (device, cert)
        };
        let pre_admit = |admitter: &Admitter, person: [u8; 32], token: &'static [u8]| {
            answer_pre_admit(
                admitter.clone(),
                hex::encode(person),
                hex::encode(token_digest(token)),
                "Guest".into(),
                RoomRole::Editor,
            )
        };

        // Someone who joined while the door was open is a member. An open
        // room's link is its invitation: members invite nobody themselves.
        let member = guest(12);
        let person = member.1.user_public_key;
        assert!(
            joins(
                &rooms,
                &member,
                pre_admit(&open, person, b"open").await,
                b"open"
            )
            .await
        );
        let asked = invite(&rooms, &room, &member.1).await;
        assert!(matches!(asked, RoomsReply::Refused(_)), "{asked:?}");

        settle_member_invites(&rooms, &room, &open.host_cert, true)
            .await
            .unwrap();
        let stranger = guest(11);
        let asked = pre_admit(&by_invitation, stranger.1.user_public_key, b"stranger").await;
        assert!(
            matches!(
                asked,
                RoomEvent::PreAdmitRefused {
                    invite_only: true,
                    ..
                }
            ),
            "{asked:?}"
        );
        let another_tab = (
            SigningKey::from_bytes(&[13; 32]),
            enrol(
                &SigningKey::from_bytes(&[112; 32]),
                &SigningKey::from_bytes(&[13; 32]),
                "guest",
                DeviceCapabilities::member(),
            )
            .unwrap(),
        );
        assert_eq!(another_tab.1.user_public_key, person, "the same person");
        let asked = pre_admit(&by_invitation, person, b"again").await;
        assert!(
            joins(&rooms, &another_tab, asked, b"again").await,
            "a member gets in again"
        );

        // The member invites the stranger, who asks to join with it and
        // waits until the member lets them in.
        let issued = invite(&rooms, &room, &member.1).await;
        let RoomsReply::Invitation {
            signed_invitation, ..
        } = issued
        else {
            panic!("a member invites: {issued:?}");
        };
        let JoinReply::Challenge { challenge } = rooms
            .page_join(JoinStep::Ask {
                invitation: signed_invitation,
                device_cert: stranger.1.clone(),
                request_id: rand::random(),
            })
            .await
        else {
            panic!("no challenge");
        };
        let signature = stranger.0.sign(&join_proof_digest(&challenge)).to_bytes();
        let waiting = rooms
            .page_join(JoinStep::Prove {
                challenge,
                signature,
                name: "Stranger".into(),
            })
            .await;
        assert!(matches!(waiting, JoinReply::Waiting), "{waiting:?}");
        let listed = rooms
            .page_rooms(&member.1, RoomsRequest::Requests { room: room.clone() })
            .await;
        let RoomsReply::Requests(requests) = listed else {
            panic!("the member sees who waits: {listed:?}");
        };
        let [request] = requests.as_slice() else {
            panic!("{requests:?}");
        };
        assert_eq!(request.user_public_key, stranger.1.user_public_key);
        let admitted = rooms
            .page_rooms(
                &member.1,
                RoomsRequest::Admit {
                    room: room.clone(),
                    request_id: request.request_id,
                },
            )
            .await;
        assert!(matches!(admitted, RoomsReply::Room(_)), "{admitted:?}");
        let members = rooms.summary(&room).await.unwrap().members;
        assert!(
            members.iter().any(
                |member| member.user_public_key == stranger.1.user_public_key && !member.removed
            ),
            "the invited stranger is a member"
        );

        settle_member_invites(&rooms, &room, &open.host_cert, false)
            .await
            .unwrap();
        let asked = invite(&rooms, &room, &member.1).await;
        assert!(matches!(asked, RoomsReply::Refused(_)), "{asked:?}");
        relay.shutdown().await;
    }

    #[test]
    fn declared_owners_are_promoted_once_they_joined_and_dropped_keys_are_demoted() {
        let owners = BTreeSet::from([[1; 32], [9; 32]]);
        // [9] has not joined yet: nothing to do for them until they do.
        let changes = role_changes(
            &[member(1, false), member(2, true)],
            &owners,
            &BTreeSet::new(),
            &BTreeSet::from([[2; 32]]),
        );
        assert_eq!(
            changes,
            vec![
                RoleChange::Administrator([1; 32]),
                RoleChange::Editor([2; 32])
            ]
        );
        assert!(
            role_changes(
                &[member(1, true)],
                &owners,
                &BTreeSet::new(),
                &BTreeSet::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn a_declared_moderator_is_promoted_and_demoted_when_dropped() {
        let moderators = BTreeSet::from([[1; 32]]);
        assert_eq!(
            role_changes(
                &[member(1, false)],
                &BTreeSet::new(),
                &moderators,
                &BTreeSet::new()
            ),
            vec![RoleChange::Moderator([1; 32])]
        );
        let moderator = tilo_peer_state::GovernedMember {
            role: tilo_peer_state::join::MemberRole::Moderator,
            ..member(1, false)
        };
        assert!(
            role_changes(
                std::slice::from_ref(&moderator),
                &BTreeSet::new(),
                &moderators,
                &BTreeSet::new()
            )
            .is_empty()
        );
        assert_eq!(
            role_changes(
                &[moderator],
                &BTreeSet::new(),
                &BTreeSet::new(),
                &moderators
            ),
            vec![RoleChange::Editor([1; 32])]
        );
    }

    #[test]
    fn pruned_guests_are_not_reported_as_removed() {
        let now = 40 * DAY_MS;
        let last_seen = BTreeMap::from([
            ([1; 32], now - 61 * MINUTE_MS),
            ([2; 32], now - 59 * MINUTE_MS),
        ]);
        let keep = BTreeSet::from([[3; 32]]);
        let members = [member(1, false), member(2, false), member(3, false)];
        let idle = idle_guests(&members, &last_seen, &keep, now, 60);
        assert_eq!(idle, vec![[1; 32]]);
        // The operator loop adds pruned keys to `removed` before the
        // governance change arrives, so newly_removed never reports them.
        let mut seen: BTreeSet<[u8; 32]> = idle.iter().copied().collect();
        let mut gone = member(1, false);
        gone.removed = true;
        assert!(newly_removed(&mut seen, &[gone]).is_empty());
    }

    #[test]
    fn a_member_away_for_an_hour_or_a_week_stays_one_and_thirty_days_away_does_not() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Args {
            #[command(flatten)]
            persistent: PersistentArgs,
        }
        let days = Args::parse_from(["relay"]).persistent.inactive_after_days;
        assert_eq!(days, 30, "the host's default");
        let horizon = days * 24 * 60;
        let now = 400 * DAY_MS;
        let last_seen = BTreeMap::from([
            ([1; 32], now - 61 * MINUTE_MS),
            ([2; 32], now - 7 * DAY_MS),
            ([3; 32], now - 29 * DAY_MS),
            ([4; 32], now - 31 * DAY_MS),
        ]);
        let members = [
            member(1, false),
            member(2, false),
            member(3, false),
            member(4, false),
        ];
        assert_eq!(
            idle_guests(&members, &last_seen, &BTreeSet::new(), now, horizon),
            vec![[4; 32]],
            "only thirty days away ends a membership"
        );
        let shorter = Args::parse_from(["relay", "--inactive-after-days", "7"])
            .persistent
            .inactive_after_days;
        assert_eq!(shorter, 7, "the host configures it");
        assert!(Args::try_parse_from(["relay", "--inactive-after-days", "0"]).is_err());
    }

    /// A site's room lets everyone in through its door: unless its host says
    /// otherwise, each member's windows stay on their device.
    #[test]
    fn a_site_rooms_layout_is_personal_unless_its_host_says_otherwise() {
        use clap::Parser as _;
        use tilo_desktop_model::projection::RoomLayout;
        #[derive(clap::Parser)]
        struct Args {
            #[command(flatten)]
            persistent: PersistentArgs,
        }
        assert_eq!(
            Args::parse_from(["relay"]).persistent.layout,
            RoomLayout::Personal
        );
        assert_eq!(
            Args::parse_from(["relay", "--layout", "shared"])
                .persistent
                .layout,
            RoomLayout::Shared
        );
        assert!(Args::try_parse_from(["relay", "--layout", "public"]).is_err());
    }

    #[test]
    fn a_guest_never_seen_counts_from_now_not_from_the_epoch() {
        let members = [member(1, false)];
        let idle = idle_guests(
            &members,
            &BTreeMap::new(),
            &BTreeSet::new(),
            40 * DAY_MS,
            60,
        );
        assert!(
            idle.is_empty(),
            "a member with no record yet gets a record, not a removal"
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
