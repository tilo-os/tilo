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
use tilo_relay::rooms::{PreAdmitRequest, Rooms};
use tilo_relay::web::WebHandle;
use tilo_relay_protocol::room_operator::{self, OperatorCommand, RoomEvent, hex32};
use tilo_relay_protocol::rooms::{JoinReply, JoinStep, PreAdmission, RoomRole, join_proof_digest};
use tilo_relay_session::rooms::SetMemberRoleRequest;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use crate::web_cli::{RoomArgs, enrol, operator_lines, start_relay, web_config};

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
        value_parser = crate::room_seed::channel_name
    )]
    pub chat_channels: Vec<String>,
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
    /// A file of more owners and moderators, `{"owners": [hex],
    /// "moderators": [hex]}`, read again whenever it changes, so a role is
    /// granted or taken back while the room runs.
    #[arg(long = "roles-file", value_name = "PATH")]
    pub roles_file: Option<PathBuf>,
    /// An app authored into the room at its first start; repeatable.
    #[arg(long = "seed-app", value_name = "APP_ID")]
    pub seed_apps: Vec<String>,
    /// Every this many seconds, snapshot each seeded app with starting
    /// content (not chat) and reset it to that content; 0 never resets.
    #[arg(long = "reset-every", value_name = "SECS", default_value_t = 0)]
    pub reset_every: u64,
    /// Guests with no session for this many minutes are removed; they join
    /// again as new guests. Every join costs the room time in proportion to
    /// its members, so a public room keeps the people around now, not every
    /// visitor: past about 800 members it admits fewer than three a second.
    #[arg(long = "prune-after-mins", default_value_t = 60)]
    pub prune_after_mins: u64,
    /// A bot directory entry and its inputs (`<entry> key=value …`) to
    /// install at the room's first start, approved by the room's root;
    /// repeatable.
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
    /// for its human Moderators.
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
    pub entry: String,
    pub inputs: BTreeMap<String, String>,
}

pub(crate) fn parse_bot_spec(text: &str) -> anyhow::Result<BotSpec> {
    let mut words = text.split_whitespace();
    let entry = words
        .next()
        .context("a bot spec names a directory entry")?
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

fn parse_keys(flag: &str, keys: &[String]) -> anyhow::Result<BTreeSet<[u8; 32]>> {
    keys.iter()
        .map(|key| {
            hex32(key)
                .with_context(|| format!("--{flag} {key:?} is not 64 lowercase hex characters"))
        })
        .collect()
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

async fn answer_pre_admit(
    context: &PersistentRoom,
    user_public_key: &str,
    token_sha256: String,
    label: &str,
    role: RoomRole,
) -> RoomEvent {
    let refused = |reason: String| RoomEvent::PreAdmitRefused {
        token_sha256: token_sha256.clone(),
        reason,
    };
    let (Some(user), Some(digest)) = (hex32(user_public_key), hex32(&token_sha256)) else {
        return refused("the guest key and token digest must be 64 hex characters".into());
    };
    if user == context.spectator.verifying_key().to_bytes() {
        return refused("the spectator person joins no other way".into());
    }
    match issue_pre_admission(
        &context.rooms,
        &context.host_cert,
        &context.root,
        &context.room,
        user,
        digest,
        label,
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
        // Written before the app counts as seeded: a start interrupted
        // here writes it again, and writing the seed twice changes nothing.
        if crate::room_seed::has_seed(app) {
            rooms
                .author_application(room, cert, &catalogue, app, &record, |doc| {
                    crate::room_seed::write_seed(app, doc)
                })
                .await
                .map_err(|error| anyhow::anyhow!("write the starting content of {app}: {error}"))?;
        }
        seeded.insert(app.clone());
        write_json(seeded_file, &seeded)?;
    }
    Ok(())
}

/// Give the room's chat, when it is seeded and while it has only the
/// channels every room opens with, the operator's starting `channels`. What
/// people wrote is never touched, and a chat with channels keeps exactly
/// those.
pub(crate) async fn settle_chat(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    apps: &[String],
    channels: &[String],
) -> anyhow::Result<()> {
    if !apps.iter().any(|app| app == crate::room_seed::CHAT_APP) {
        return Ok(());
    }
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let record = tilo_relay::room_bots::room_record(&catalogue, crate::room_seed::CHAT_APP);
    rooms
        .author_application(
            room,
            cert,
            &catalogue,
            crate::room_seed::CHAT_APP,
            &record,
            |doc| crate::room_seed::settle_chat(doc, channels),
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

/// Snapshot each seeded app with starting content under
/// `<state>/snapshots/<app>/`, then reset it to that content. Chat has no
/// starting content and is never touched.
pub(crate) async fn reset_seeded(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    apps: &[String],
    state: &Path,
) -> anyhow::Result<()> {
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let seeded: BTreeSet<String> = read_json(&state.join("seeded.json"))?;
    let now = tilo_clock::unix_now_ms() / 1000;
    for app in apps
        .iter()
        .filter(|app| seeded.contains(*app) && crate::room_seed::has_seed(app))
    {
        let dir = state.join("snapshots").join(app);
        let record = tilo_relay::room_bots::room_record(&catalogue, app);
        rooms
            .author_application(room, cert, &catalogue, app, &record, |doc| {
                // The snapshot is taken under the same lock as the reset, so
                // it is exactly what the reset replaced.
                crate::room_seed::keep_snapshot(&dir, now, &doc.save())
                    .map_err(|error| format!("snapshot: {error}"))?;
                crate::room_seed::write_seed(app, doc)
            })
            .await
            .map_err(|error| anyhow::anyhow!("reset {app}: {error}"))?;
    }
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
    let hosted = start_relay(
        config,
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host,
            device_cert: host_cert.clone(),
            revocations: RevocationList::new(),
        },
        &args.web,
        &state,
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
    )
    .await?;
    settle_strips(&rooms, &room, &host_cert, &shown, &persistent.strips).await?;
    ensure_guest_policy(&rooms, &room, &host_cert).await?;
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
    let runners = start_bots(
        &rooms,
        &room,
        &host_cert,
        &root,
        &bots,
        &workers,
        &persistent,
        &state,
    )
    .await?;
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
        prune_after_mins: persistent.prune_after_mins,
        state: state.clone(),
        seed_apps: persistent.seed_apps.clone(),
        reset_every: persistent.reset_every,
    };
    let result = tokio::select! {
        result = serve_operator(&context, &governance, &mut operator) => result,
        result = shutdown => result,
    };
    for runner in moderator.into_iter().chain(runners) {
        runner.abort();
    }
    drop(context);
    hosted.shutdown().await;
    result
}

/// Connect the room to the Discord server a bridge's inputs name, and keep
/// that link with the host: the worker's settings come back, and the link
/// inputs leave the program's. Without a server the bridge would act in
/// whatever servers the bot is in, so it does not run.
async fn link_discord(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    spec: &BotSpec,
    bot: &mut tilo_relay::room_bots::installed::InstalledBot,
    persistent: &PersistentArgs,
) -> Result<Vec<(String, String)>, String> {
    use tilo_relay::room_bots::discord;
    let link = discord::DiscordLink::from_inputs(&spec.inputs)?
        .ok_or("a Discord bridge needs guild=<server id>")?;
    discord::connect(rooms, room, host_cert, &link).await?;
    for key in discord::LINK_INPUTS {
        bot.inputs.remove(key);
    }
    // Authors bridged out show the avatars this host publishes, unless the
    // operator pointed them elsewhere.
    if let (false, Some(url)) = (
        bot.inputs.contains_key("context"),
        &persistent.public_avatars_url,
    ) {
        bot.inputs.insert(
            "context".to_owned(),
            serde_json::json!({ "avatars": format!("{}/", url.trim_end_matches('/')) }),
        );
    }
    Ok(link.worker_env())
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
    workers: &BTreeMap<String, PathBuf>,
    persistent: &PersistentArgs,
    state: &Path,
) -> anyhow::Result<Vec<tokio::task::JoinHandle<()>>> {
    use tilo_relay::room_bots::discord;
    use tilo_relay::room_bots::installed::{self, BotRunner, InstalledBot};
    if let Some(path) = &persistent.discord_token_file {
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("read the Discord token file {}", path.display()))?;
        // Never logged: only kept where the connector's worker asks for it.
        installed::put_connector_secret(discord::APP, "bot-token", token.trim().as_bytes())
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
            match link_discord(rooms, room, host_cert, spec, &mut bot, persistent).await {
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
    let public = match (&persistent.public_avatars, &persistent.public_avatars_url) {
        (Some(dir), Some(url)) => Some(
            tilo_relay::public_avatars::PublicAvatars::new(dir.clone(), url)
                .with_context(|| format!("prepare {}", dir.display()))?,
        ),
        _ => None,
    };
    let kept = Arc::new(
        installed::FsBlobs::new(state.join("blobs"), public).context("prepare the blob store")?,
    );
    rooms.serve_content(kept.clone());
    let blobs: Arc<dyn tilo_connector::BlobAccess> = kept;
    let started = installed::start_connectors(&connectors, state, workers, &settings, &blobs)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut runners = Vec::new();
    for (dir, bot) in installed_bots {
        let wanted =
            InstalledBot::connectors(&bot.shortcut().map_err(|error| anyhow::anyhow!("{error}"))?);
        let own = started
            .iter()
            .filter(|(app, _)| wanted.contains(*app))
            .map(|(app, process)| (app.clone(), Arc::clone(process)))
            .collect();
        let certifier = tilo_relay::room_bots::membership::HostRootCertifier::new(root.clone());
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
    prune_after_mins: u64,
    state: PathBuf,
    seed_apps: Vec<String>,
    /// Seconds between resets of seeded content; 0 never resets.
    reset_every: u64,
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

/// Ticks every `secs`, the first one period after the start, not at it: a
/// restart must not wipe what visitors just made.
fn reset_interval(secs: u64) -> tokio::time::Interval {
    let period = Duration::from_secs(secs.max(1));
    let now = tokio::time::Instant::now();
    tokio::time::interval_at(now.checked_add(period).unwrap_or(now), period)
}

/// Reset the seeded content. A failed reset leaves the content as it is
/// until the next one; the room keeps serving.
async fn reset_now(context: &PersistentRoom, governance: &Governance) {
    if let Err(error) = reset_seeded(
        &context.rooms,
        &context.room,
        &context.host_cert,
        &governance.seed_apps,
        &governance.state,
    )
    .await
    {
        tracing::warn!("resetting the room's content failed: {error:#}");
    }
}

/// Removes guests idle past the horizon, in batches. A batch the room
/// refuses stays seated until the next call.
async fn prune_idle(
    context: &PersistentRoom,
    governance: &Governance,
    spectator: [u8; 32],
    removed: &mut BTreeSet<[u8; 32]>,
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
    let idle = idle_guests(&members, last_seen, &keep, now, governance.prune_after_mins);
    for batch in idle.chunks(PRUNE_BATCH) {
        let batch: BTreeSet<[u8; 32]> = batch.iter().copied().collect();
        // Into `removed` first: a pruned guest is not a removal the service
        // should cool their address down for.
        removed.extend(batch.iter().copied());
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
            // the room goes on serving, as after a failed reset: ending
            // here dropped every visitor's session and turned each new one
            // away while the room service restarted it.
            for key in &batch {
                removed.remove(key);
            }
            tracing::warn!("pruning {} idle guests failed: {error}", batch.len());
            break;
        }
        for key in &batch {
            last_seen.remove(key);
        }
    }
    governance.write_last_seen(last_seen)
}

/// How often the operator's roles file is looked at for a change.
const ROLES_FILE_EVERY_SECS: u64 = 2;

async fn serve_operator(
    context: &PersistentRoom,
    governance: &Governance,
    operator: &mut tokio::sync::mpsc::Receiver<Result<OperatorCommand, String>>,
) -> anyhow::Result<()> {
    let mut changes = context.rooms.governance_changes();
    let spectator = context.spectator.verifying_key().to_bytes();
    let initial = summary(context).await?.members;
    let mut removed: BTreeSet<[u8; 32]> = initial
        .iter()
        .filter(|member| member.removed)
        .map(|member| member.user_public_key)
        .collect();
    let mut previously_declared = governance.previously_declared()?;
    reconcile_roles(context, governance, &mut previously_declared).await?;
    let mut last_seen = governance.read_last_seen()?;
    let mut last_seen_written = tokio::time::Instant::now();
    let mut online_tick =
        tokio::time::interval(Duration::from_secs(room_operator::ONLINE_EVERY_SECS));
    // A quarter of the horizon, so nobody stays much past it, and not more
    // often than a minute.
    let mut prune_tick = tokio::time::interval(Duration::from_secs(
        governance
            .prune_after_mins
            .saturating_mul(15)
            .clamp(60, 3600),
    ));
    let mut reset_tick = reset_interval(governance.reset_every);
    let mut last_online = None;
    // The operator's roles file, looked at every few seconds: a role it
    // grants or takes back reaches the room without a restart.
    let mut roles_tick = tokio::time::interval(Duration::from_secs(ROLES_FILE_EVERY_SECS));
    let mut roles_seen = governance.roles_changed_at();
    loop {
        tokio::select! {
            _ = roles_tick.tick(), if governance.roles_file.is_some() => {
                let changed_at = governance.roles_changed_at();
                if changed_at != roles_seen {
                    roles_seen = changed_at;
                    reconcile_roles(context, governance, &mut previously_declared).await?;
                }
            }
            _ = online_tick.tick() => {
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
                let here: std::collections::BTreeSet<[u8; 32]> =
                    sessions.keys().filter(|user| **user != spectator).copied().collect();
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
                if last_online.as_ref() != Some(&online) {
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
                    last_online = Some(online);
                }
                let now = tilo_clock::unix_now_ms();
                for user in sessions.keys() {
                    last_seen.insert(*user, now);
                }
                if last_seen_written.elapsed() >= Duration::from_secs(60) {
                    for member in summary(context).await?.members.iter().filter(|member| !member.removed) {
                        last_seen.entry(member.user_public_key).or_insert(now);
                    }
                    governance.write_last_seen(&last_seen)?;
                    last_seen_written = tokio::time::Instant::now();
                }
            }
            _ = prune_tick.tick() => prune_idle(context, governance, spectator, &mut removed, &mut last_seen).await?,
            _ = reset_tick.tick(), if governance.reset_every > 0 => reset_now(context, governance).await,
            changed = changes.recv() => {
                match changed {
                    Ok(room) if room != context.room => continue,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => bail!("the relay stopped governing"),
                }
                let members = summary(context).await?.members;
                for user in newly_removed(&mut removed, &members) {
                    // Bans exist once room moderation lands; until then every
                    // removal is a plain removal. The service treats both alike.
                    println!("{}", RoomEvent::MemberRemoved { user_public_key: hex::encode(user), banned: false }.to_line());
                }
                reconcile_roles(context, governance, &mut previously_declared).await?;
            }
            line = operator.recv() => match line {
                Some(Ok(OperatorCommand::PreAdmit { user_public_key, token_sha256, label, role })) => {
                    let event = answer_pre_admit(context, &user_public_key, token_sha256, &label, role).await;
                    println!("{}", event.to_line());
                }
                Some(Ok(OperatorCommand::Spectate { device_public_key })) => {
                    println!("{}", answer_spectate(context, device_public_key).to_line());
                }
                Some(Ok(OperatorCommand::Admit { .. })) => bail!("a persistent room admits guests by pre-admission only"),
                Some(Ok(OperatorCommand::Shutdown {})) | None => return Ok(()),
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
