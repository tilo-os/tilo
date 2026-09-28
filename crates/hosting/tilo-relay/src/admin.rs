//! `tilo-relay-admin` — thin CLI to bind/unbind workspaces + inspect
//! the running daemon by reading its on-disk state.
//!
//! Mutations are durable file updates which a running daemon reloads.
//! `status` combines configuration, workspace state, and the daemon's
//! periodically persisted metrics snapshot.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use tilo_relay::RelayConfig;

const DEFAULT_CONFIG_PATH: &str = "/etc/tilo/relay.toml";

#[derive(Parser)]
#[command(
    name = "tilo-relay-admin",
    version,
    about = "Administer a tilo-relay daemon"
)]
struct Cli {
    /// Path to the relay TOML config.
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Retry durable removals queued while a home peer was offline.
    HomePeerRetryRemovals {
        #[arg(long)]
        identity_root: PathBuf,
    },
    /// Read authenticated health from an enrolled home peer.
    HomePeerStatus {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
    },
    /// Rename an enrolled home peer after mutual authentication.
    HomePeerRename {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        label: String,
    },
    /// Root-revoke and permanently stop an enrolled home peer.
    HomePeerRemove {
        #[arg(long)]
        identity_root: PathBuf,
        /// The personal identity directory whose user root signs the
        /// revocation, usually `~/.config/tilo/identity`.
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
    },
    /// Bind a workspace to this relay.
    BindWorkspace {
        /// Numeric workspace id.
        workspace_id: u64,
        /// Pin the workspace (persistent CRDT snapshot on disk).
        #[arg(long, default_value_t = true)]
        pin: bool,
    },
    /// Bind a user-facing room UUID, deriving its stable transport id.
    BindRoom {
        /// Durable room UUID shown by the shell.
        room: String,
        /// Pin the workspace (persistent CRDT snapshot on disk).
        #[arg(long, default_value_t = true)]
        pin: bool,
    },
    /// Unbind a workspace from this relay.
    UnbindWorkspace {
        /// Numeric workspace id.
        workspace_id: u64,
    },
    /// List bound workspaces.
    ListWorkspaces,
    /// Add a device to a workspace's durable offline-delivery membership.
    AddMember {
        /// Numeric workspace id (derive it with `bind-room`).
        workspace_id: u64,
        /// 32-byte device public key, hex encoded.
        device_pubkey_hex: String,
    },
    /// Remove a device from explicit shared-host membership. A matching-user
    /// home host will admit the owner's device again; revoke it to deny access.
    RemoveMember {
        workspace_id: u64,
        device_pubkey_hex: String,
    },
    /// List device keys admitted to a workspace.
    ListMembers { workspace_id: u64 },
    /// Print daemon status (config + on-disk state summary).
    Status,
    /// Host a public room (such as the Tilo community room) whose owner lets
    /// this relay admit people through the room's standing invitation.
    #[command(subcommand)]
    Community(CommunityCmd),
    /// Print where clients dial this relay: its Iroh endpoint address as the
    /// JSON `TILO_COLLAB_HOSTS` entries take, derived from the device key in
    /// `identity_root`. Works before the relay ever ran, so the address a
    /// standing invitation carries can name a relay that is not yet deployed.
    Endpoint {
        /// A public `IP:PORT` reaching this relay's `iroh_listen` port (a
        /// load balancer or a static address). Repeat for more; without any,
        /// clients find the relay by its id through Iroh discovery alone.
        #[arg(long = "addr")]
        addrs: Vec<std::net::SocketAddr>,
    },
    /// Publish a `RevocationEntry` for a device pubkey (hex-encoded, 32 bytes).
    ///
    /// Signs with the user root held by the personal identity in
    /// `--identity-dir`, so it runs as that identity's owner on the device that
    /// created the identity.
    RevokeDevice {
        /// Hex-encoded device pubkey to revoke.
        device_pubkey_hex: String,
        /// The personal identity directory holding the active record
        /// (`primary.json`), usually `~/.config/tilo/identity`.
        #[arg(long)]
        identity_dir: PathBuf,
        /// lost, stolen, retired, or compromised.
        #[arg(long, default_value = "compromised")]
        reason: String,
    },
}

/// A public room is created — and its governance written — by its owner's
/// shell; see `docs/architecture/feedback.md`. These commands make this relay
/// the always-on host that lets people join it while the owner is away.
///
/// The usual order:
///
///   1. In the owner's shell, create the public room and copy its id.
///   2. `community host --room ROOM --owner-identity-dir DIR` here, as the
///      owner: binds the room and stores the owner's signed delegation for
///      this relay.
///   3. Restart tilo-relay so it serves the newly bound room, with
///      `trust_policy = { kind = "allow_any" }` so certified strangers may
///      connect (every room stays gated by its own membership).
///   4. In the owner's shell, mint the room's standing invitation with this
///      relay as its host, then `community add-invite --room ROOM TOKEN` here.
///   5. Ship TOKEN (in the image's `community.toml`); anyone redeems it.
///
/// `community revoke-invite` stops further redemptions at once; people already
/// admitted stay members. `community status` shows what is hosted.
///
/// When the owner's identity and the serving relay are on different machines
/// (a hosted production relay), steps 2 and 4 run against a copy of the
/// relay's configuration on the owner's machine, and the result travels as
/// one record: `community export --room ROOM` there, `community install
/// --record FILE` on the serving relay. Only public material moves: the
/// delegation and the invitations, never the owner's root.
#[derive(Subcommand)]
enum CommunityCmd {
    /// Bind a public room here and store its owner's delegation.
    ///
    /// Signs, with the owner's user root, a delegation letting this relay's
    /// device admit redeeming devices into ROOM at role Commenter and nothing
    /// more. The root stays in the owner's personal identity record in DIR
    /// and is not copied here.
    Host {
        /// The public room's UUID, as the owner's shell shows it.
        #[arg(long)]
        room: String,
        /// The room owner's personal identity directory, holding the active
        /// record (`primary.json`), usually `~/.config/tilo/identity`. Run
        /// this as the owner.
        #[arg(long)]
        owner_identity_dir: PathBuf,
        /// End the delegation after this many days. Without it the delegation
        /// lasts until replaced; admissions stop verifying once it ends.
        #[arg(long)]
        expires_in_days: Option<u64>,
    },
    /// Register a standing invitation of a hosted room for redemption here.
    ///
    /// TOKEN must be a standing invitation to ROOM minted by a device of the
    /// room's owner; it can grant at most Commenter.
    AddInvite {
        /// The hosted room's UUID.
        #[arg(long)]
        room: String,
        /// The standing invitation, as the owner's shell printed it.
        token: String,
    },
    /// Stop redeeming a standing invitation. People it admitted stay members.
    RevokeInvite {
        /// The hosted room's UUID.
        #[arg(long)]
        room: String,
        /// The invitation's id, as `community status` lists it (hex).
        invite_id: String,
    },
    /// Show a hosted public room: its owner, delegation, invitations and
    /// admitted device count.
    Status {
        /// The hosted room's UUID.
        #[arg(long)]
        room: String,
    },
    /// Print a hosted room's record — its owner, the delegation and every
    /// standing invitation with its revocation — as JSON for `community
    /// install` on the relay that serves it. Holds no secret.
    Export {
        /// The hosted room's UUID.
        #[arg(long)]
        room: String,
    },
    /// Host a public room from a record `community export` wrote elsewhere.
    ///
    /// The delegation must name this relay's device; every live invitation
    /// must be the room's standing invitation from a device of its owner.
    /// Binds the room, replaces the delegation and merges the invitations:
    /// a revocation on either side stays. Running it again is a no-op.
    Install {
        /// The exported record (JSON).
        #[arg(long)]
        record: PathBuf,
    },
}

fn load(path: &std::path::Path) -> anyhow::Result<RelayConfig> {
    if path.exists() {
        Ok(RelayConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(RelayConfig::default())
    }
}

fn device_key(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(value.trim()).with_context(|| "device pubkey must be hex")?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("device pubkey must be 32 bytes; got {}", bytes.len())
    })
}

fn workspace_store(
    cfg: &RelayConfig,
    workspace_id: u64,
) -> anyhow::Result<tilo_relay::store::WorkspaceStore> {
    let binding = cfg
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} is not bound"))?;
    tilo_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, binding.pin)
        .map_err(Into::into)
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::HomePeerRetryRemovals { identity_root } => retry_pending_removals(&identity_root),
        Cmd::HomePeerStatus {
            identity_root,
            device,
            service,
        } => cmd_home_peer(&identity_root, &device, &service, None),
        Cmd::HomePeerRename {
            identity_root,
            device,
            service,
            label,
        } => cmd_home_peer(&identity_root, &device, &service, Some(label)),
        Cmd::HomePeerRemove {
            identity_root,
            identity_dir,
            device,
            service,
        } => cmd_home_peer_remove(&identity_root, &identity_dir, &device, &service),
        Cmd::BindWorkspace { workspace_id, pin } => {
            cmd_bind_workspace(&cli.config, workspace_id, pin)
        }
        Cmd::BindRoom { room, pin } => cmd_bind_room(&cli.config, &room, pin),
        Cmd::UnbindWorkspace { workspace_id } => cmd_unbind_workspace(&cli.config, workspace_id),
        Cmd::ListWorkspaces => cmd_list_workspaces(&cli.config),
        Cmd::AddMember {
            workspace_id,
            device_pubkey_hex,
        } => cmd_add_member(&cli.config, workspace_id, &device_pubkey_hex),
        Cmd::RemoveMember {
            workspace_id,
            device_pubkey_hex,
        } => cmd_remove_member(&cli.config, workspace_id, &device_pubkey_hex),
        Cmd::ListMembers { workspace_id } => cmd_list_members(&cli.config, workspace_id),
        Cmd::Status => cmd_status(&cli.config),
        Cmd::RevokeDevice {
            device_pubkey_hex,
            identity_dir,
            reason,
        } => cmd_revoke_device(&cli.config, &identity_dir, &device_pubkey_hex, &reason),
        Cmd::Community(cmd) => cmd_community(&cli.config, cmd),
        Cmd::Endpoint { addrs } => cmd_endpoint(&cli.config, &addrs),
    }
}

fn relay_device_cert(cfg: &RelayConfig) -> anyhow::Result<tilo_user_identity::DeviceCert> {
    let paths = tilo_user_identity::IdentityPaths::under(&cfg.identity_root);
    tilo_user_identity::persist::read_device_cert(&paths.device_cert).with_context(|| {
        format!(
            "read this relay's device certificate {}",
            paths.device_cert.display()
        )
    })
}

fn cmd_endpoint(config: &std::path::Path, addrs: &[std::net::SocketAddr]) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let paths = tilo_user_identity::IdentityPaths::under(&cfg.identity_root);
    let device_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
        .with_context(|| {
            format!(
                "read this relay's device key {}",
                paths.device_privkey.display()
            )
        })?;
    let id = iroh::EndpointId::from_bytes(&tilo_relay::daemon::relay_service_node_id(&device_key))
        .context("derive the relay's endpoint id")?;
    let addr = addrs
        .iter()
        .fold(iroh::EndpointAddr::new(id), |addr, socket| {
            addr.with_ip_addr(*socket)
        });
    println!("{}", serde_json::to_string(&addr)?);
    Ok(())
}

fn hosted_room_store(
    cfg: &RelayConfig,
    room: &str,
) -> anyhow::Result<(u64, tilo_relay::store::WorkspaceStore)> {
    let workspace_id = tilo_relay::room_workspace_id(room)
        .with_context(|| format!("invalid room UUID {room:?}"))?;
    let store = workspace_store(cfg, workspace_id)
        .with_context(|| format!("room {room} is not hosted here; run `community host` first"))?;
    Ok((workspace_id, store))
}

fn load_hosting(
    store: &tilo_relay::store::WorkspaceStore,
    room: &str,
) -> anyhow::Result<tilo_relay::public_room::PublicRoomHosting> {
    tilo_relay::public_room::PublicRoomHosting::load(store.root())?
        .with_context(|| format!("room {room} has no delegation here; run `community host` first"))
}

fn cmd_community(config: &std::path::Path, cmd: CommunityCmd) -> anyhow::Result<()> {
    match cmd {
        CommunityCmd::Host {
            room,
            owner_identity_dir,
            expires_in_days,
        } => cmd_community_host(config, &room, &owner_identity_dir, expires_in_days),
        CommunityCmd::AddInvite { room, token } => {
            let cfg = load(config)?;
            let (_, store) = hosted_room_store(&cfg, &room)?;
            let mut hosting = load_hosting(&store, &room)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let parsed = hosting.check_invite(&token, now)?;
            let id = hex::encode(neighbour_wire::invite::invite_id(&token));
            if hosting.invites.iter().any(|invite| invite.id == id) {
                println!("standing invitation {id} is already registered");
                return Ok(());
            }
            hosting
                .invites
                .push(tilo_relay::public_room::HostedStandingInvite {
                    id: id.clone(),
                    token: token.trim().to_string(),
                    revoked: false,
                });
            hosting.save(store.root())?;
            println!(
                "registered standing invitation {id} ({}{})",
                parsed.role.label(),
                if parsed.expiry_unix_seconds == u64::MAX {
                    String::new()
                } else {
                    format!(", expires at unix {}", parsed.expiry_unix_seconds)
                }
            );
            println!("a running tilo-relay redeems it from the next redemption on");
            Ok(())
        }
        CommunityCmd::RevokeInvite { room, invite_id } => {
            let cfg = load(config)?;
            let (_, store) = hosted_room_store(&cfg, &room)?;
            let mut hosting = load_hosting(&store, &room)?;
            let id = invite_id.trim().to_ascii_lowercase();
            let Some(invite) = hosting.invites.iter_mut().find(|invite| invite.id == id) else {
                anyhow::bail!("no standing invitation {id} is registered for room {room}");
            };
            invite.revoked = true;
            hosting.save(store.root())?;
            println!("revoked standing invitation {id}; people it admitted stay members");
            Ok(())
        }
        CommunityCmd::Status { room } => {
            let cfg = load(config)?;
            let (workspace_id, store) = hosted_room_store(&cfg, &room)?;
            let hosting = load_hosting(&store, &room)?;
            println!("room {room} (workspace {workspace_id})");
            println!("owner {}", hex::encode(hosting.owner));
            let delegation_end = hosting
                .delegation
                .iter()
                .filter_map(|token| token.not_after)
                .min();
            match delegation_end {
                Some(end) => println!(
                    "delegation ends at unix {}",
                    end.duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_secs())
                ),
                None => println!("delegation does not expire"),
            }
            for invite in &hosting.invites {
                println!(
                    "invite {}\t{}",
                    invite.id,
                    if invite.revoked { "revoked" } else { "live" }
                );
            }
            println!("admitted devices {}", store.load_membership()?.len());
            Ok(())
        }
        CommunityCmd::Export { room } => {
            let cfg = load(config)?;
            let (_, store) = hosted_room_store(&cfg, &room)?;
            let hosting = load_hosting(&store, &room)?;
            println!("{}", serde_json::to_string_pretty(&hosting)?);
            Ok(())
        }
        CommunityCmd::Install { record } => cmd_community_install(config, &record),
    }
}

fn cmd_community_install(config: &std::path::Path, record: &std::path::Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(record).with_context(|| format!("read {}", record.display()))?;
    let incoming: tilo_relay::public_room::PublicRoomHosting = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a hosted room record", record.display()))?;
    let mut cfg = load(config)?;
    let relay_cert = relay_device_cert(&cfg)?;
    let room = incoming.room.clone();
    let workspace_id = tilo_relay::room_workspace_id(&room)
        .with_context(|| format!("invalid room UUID {room:?}"))?;
    let added = cfg.add_workspace(workspace_id, false);
    let store = tilo_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, false)?;
    let existing = tilo_relay::public_room::PublicRoomHosting::load(store.root())?;
    let (_, merged) = tilo_relay::public_room::PublicRoomHosting::adopt(
        existing.clone(),
        incoming,
        relay_cert.device_public_key,
        std::time::SystemTime::now(),
    )?;
    if added {
        cfg.save(config)?;
    }
    if existing.as_ref() == Some(&merged) {
        println!("room {room} is already hosted here exactly as the record says");
    } else {
        merged.save(store.root())?;
        let (revoked, live): (Vec<_>, Vec<_>) =
            merged.invites.iter().partition(|invite| invite.revoked);
        println!(
            "hosting public room {room} as workspace {workspace_id} for owner {}: \
             {} live and {} revoked standing invitations",
            hex::encode(merged.owner),
            live.len(),
            revoked.len()
        );
    }
    if added {
        println!("restart tilo-relay to serve the newly bound room");
    }
    Ok(())
}

fn cmd_community_host(
    config: &std::path::Path,
    room: &str,
    owner_identity_dir: &std::path::Path,
    expires_in_days: Option<u64>,
) -> anyhow::Result<()> {
    let workspace_id = tilo_relay::room_workspace_id(room)
        .with_context(|| format!("invalid room UUID {room:?}"))?;
    let mut cfg = load(config)?;
    let relay_paths = tilo_user_identity::IdentityPaths::under(&cfg.identity_root);
    let relay_cert = tilo_user_identity::persist::read_device_cert(&relay_paths.device_cert)
        .with_context(|| {
            format!(
                "read this relay's device certificate {}",
                relay_paths.device_cert.display()
            )
        })?;
    let owner = runtime_identity::load_root_signer(owner_identity_dir)
        .context("--owner-identity-dir must hold the room owner's personal identity")?;
    let owner_key = owner.group_public_key();
    let not_after = match expires_in_days {
        Some(days) => Some(
            std::time::SystemTime::now()
                .checked_add(std::time::Duration::from_secs(days.saturating_mul(86_400)))
                .context("--expires-in-days is too far in the future")?,
        ),
        None => None,
    };
    let delegation = collab_types::mint_root_with(
        collab_types::InviteSpec {
            subject: collab_types::Ed25519PublicKey::from_bytes(relay_cert.device_public_key),
            workspace: workspace_id,
            role: collab_types::Role::Commenter,
            caps: std::collections::BTreeSet::from([
                collab_types::caps::Verb::Read,
                collab_types::caps::Verb::Comment,
            ]),
            sub_resource: None,
            not_after,
        },
        collab_types::Ed25519PublicKey::from_bytes(owner_key),
        |bytes| owner.sign(bytes).map_err(|error| error.to_string()),
    )?;
    tilo_relay::public_room::PublicRoomHosting::check_delegation(
        room,
        owner_key,
        std::slice::from_ref(&delegation),
        relay_cert.device_public_key,
        std::time::SystemTime::now(),
    )?;
    let added = cfg.add_workspace(workspace_id, false);
    cfg.save(config)?;
    let store = workspace_store(&cfg, workspace_id)?;
    let mut hosting = tilo_relay::public_room::PublicRoomHosting::load(store.root())?
        .unwrap_or_else(|| tilo_relay::public_room::PublicRoomHosting {
            room: room.to_string(),
            owner: owner_key,
            delegation: Vec::new(),
            invites: Vec::new(),
        });
    if hosting.owner != owner_key || hosting.room != room {
        anyhow::bail!(
            "room {room} is already hosted here for owner {}; unbind it first",
            hex::encode(hosting.owner)
        );
    }
    hosting.delegation = vec![delegation];
    hosting.save(store.root())?;
    println!(
        "hosting public room {room} as workspace {workspace_id} for owner {}",
        hex::encode(owner_key)
    );
    if added {
        println!("restart tilo-relay to serve the newly bound room");
    }
    if cfg.trust_policy != tilo_relay::TrustPolicy::AllowAny {
        println!(
            "note: trust_policy is not {{ kind = \"allow_any\" }}, so only devices it admits can connect and redeem"
        );
    }
    Ok(())
}

fn remote_home_peer(
    identity_root: &std::path::Path,
    device: &str,
    service: &str,
    request: tilo_relay::transport::HomePeerManagementRequest,
) -> anyhow::Result<tilo_relay::transport::HomePeerManagementResponse> {
    let paths = tilo_user_identity::IdentityPaths::under(identity_root);
    let user = tilo_user_identity::persist::read_public_key(&paths.user_pubkey)?;
    let signing_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))?;
    let expected_device = device_key(device)?;
    let service = device_key(service)?;
    let relay_addr = registered_home_peer_addr(identity_root, device, service)?;
    tilo_rt::block_on(async move {
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tilo_relay::client::RelayClient::manage_home_peer(
                &endpoint,
                relay_addr,
                user,
                expected_device,
                cert,
                signing_key,
                tilo_user_identity::RevocationList::new(),
                request,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("home peer did not answer within 10 seconds"))??;
        endpoint.close().await;
        Ok(response)
    })?
}

fn cmd_home_peer(
    identity_root: &std::path::Path,
    device: &str,
    service: &str,
    rename: Option<String>,
) -> anyhow::Result<()> {
    let requested_label = rename.clone();
    let request = rename.map_or(
        tilo_relay::transport::HomePeerManagementRequest::Status,
        |label| tilo_relay::transport::HomePeerManagementRequest::Rename { label },
    );
    let response = remote_home_peer(identity_root, device, service, request)?;
    if let Some(label) = requested_label {
        update_home_peer_registry(identity_root, device, Some(label))?;
    }
    refresh_home_peer_endpoint(
        identity_root,
        device,
        device_key(service)?,
        &response.relay_endpoint,
    )?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn registered_home_peer_addr(
    identity_root: &std::path::Path,
    device: &str,
    service: [u8; 32],
) -> anyhow::Result<iroh_base::EndpointAddr> {
    let service_id = iroh_base::PublicKey::from_bytes(&service)?;
    let path = identity_root.join("home-peers.json");
    let registrations: Vec<tilo_relay::HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(path)?)?;
    let registration = registrations
        .into_iter()
        .find(|entry| {
            entry.device_node_id == device && entry.relay_service_node_id == hex::encode(service)
        })
        .ok_or_else(|| anyhow::anyhow!("home peer is not registered"))?;
    Ok(registration
        .relay_endpoint
        .filter(|endpoint| endpoint.id == service_id && !endpoint.is_empty())
        .unwrap_or_else(|| iroh_base::EndpointAddr::new(service_id)))
}

fn refresh_home_peer_endpoint(
    identity_root: &std::path::Path,
    device: &str,
    service: [u8; 32],
    endpoint: &iroh_base::EndpointAddr,
) -> anyhow::Result<()> {
    if endpoint.id.as_bytes() != &service || endpoint.is_empty() {
        anyhow::bail!("home peer returned an invalid relay endpoint advertisement");
    }
    let path = identity_root.join("home-peers.json");
    let mut registrations: Vec<tilo_relay::HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(&path)?)?;
    let registration = registrations
        .iter_mut()
        .find(|entry| {
            entry.device_node_id == device && entry.relay_service_node_id == hex::encode(service)
        })
        .ok_or_else(|| anyhow::anyhow!("home peer is not registered"))?;
    registration.relay_endpoint = Some(endpoint.clone());
    registration.schema_version = tilo_relay::HomePeerRegistration::CURRENT_SCHEMA;
    atomic_file::write_with_mode(&path, &serde_json::to_vec_pretty(&registrations)?, 0o644)?;
    Ok(())
}

fn cmd_home_peer_remove(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    device: &str,
    service: &str,
) -> anyhow::Result<()> {
    let paths = tilo_user_identity::IdentityPaths::under(identity_root);
    let target = device_key(device)?;
    let mut pending = load_pending_decommissions(identity_root)?;
    let intent = if let Some(existing) = pending
        .iter()
        .find(|intent| intent.device == device)
        .cloned()
    {
        existing
    } else {
        let root = runtime_identity::load_root_signer(identity_dir)
            .context("removing a home server signs its revocation with your identity root")?;
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: root.group_public_key(),
            revoked_device_public_key: target,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_with_root(&mut revocation, &root)?;
        let intent = PendingDecommission {
            device: device.to_string(),
            service: service.to_string(),
            revocation,
        };
        pending.push(intent.clone());
        save_pending_decommissions(identity_root, &pending)?;
        intent
    };

    // Local denial is durable first. The management dial deliberately uses
    // an empty local server revocation view so this exact pinned certificate
    // remains reachable only long enough to deliver its own decommission.
    persist_canonical_revocation(&paths, &intent.revocation)?;
    let mut fleet_revocations = tilo_relay::store::load_revocations(identity_root)?;
    fleet_revocations.retain(|entry| entry.revoked_device_public_key != target);
    fleet_revocations.push(intent.revocation.clone());
    tilo_relay::store::save_revocations(identity_root, &fleet_revocations)?;
    persist_fleet_revocation(&intent.revocation)?;
    if identity_root.join("home-peers.json").exists() {
        update_home_peer_registry(identity_root, device, None)?;
    }

    let remote = remote_home_peer(
        identity_root,
        &intent.device,
        &intent.service,
        tilo_relay::transport::HomePeerManagementRequest::Decommission {
            revocation: intent.revocation.clone(),
        },
    );
    if remote
        .as_ref()
        .is_ok_and(|response| response.decommissioned)
    {
        pending.retain(|queued| queued.device != intent.device);
        save_pending_decommissions(identity_root, &pending)?;
    }
    let response = remote.unwrap_or(tilo_relay::transport::HomePeerManagementResponse {
        device_public_key: target,
        relay_endpoint: iroh_base::EndpointAddr::new(iroh_base::PublicKey::from_bytes(
            &device_key(&intent.service)?,
        )?),
        label: "Removed".into(),
        files_ready: false,
        services: tilo_relay_protocol::ServiceCapabilities::default(),
        decommissioned: false,
    });
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PendingDecommission {
    device: String,
    service: String,
    revocation: tilo_user_identity::RevocationEntry,
}

fn pending_decommissions_path(identity_root: &std::path::Path) -> PathBuf {
    identity_root.join("pending-home-peer-decommissions.json")
}

fn load_pending_decommissions(
    identity_root: &std::path::Path,
) -> anyhow::Result<Vec<PendingDecommission>> {
    match std::fs::read(pending_decommissions_path(identity_root)) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => Ok(serde_json::from_slice(&bytes)?),
        Ok(_) => anyhow::bail!("pending home-peer removal list is too large"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn save_pending_decommissions(
    identity_root: &std::path::Path,
    pending: &[PendingDecommission],
) -> anyhow::Result<()> {
    atomic_file::write_with_mode(
        &pending_decommissions_path(identity_root),
        &serde_json::to_vec_pretty(pending)?,
        0o600,
    )?;
    Ok(())
}

fn retry_pending_removals(identity_root: &std::path::Path) -> anyhow::Result<()> {
    let mut pending = load_pending_decommissions(identity_root)?;
    for intent in pending.clone() {
        let paths = tilo_user_identity::IdentityPaths::under(identity_root);
        let target = device_key(&intent.device)?;
        persist_canonical_revocation(&paths, &intent.revocation)?;
        let mut relay_revocations = tilo_relay::store::load_revocations(identity_root)?;
        relay_revocations.retain(|entry| entry.revoked_device_public_key != target);
        relay_revocations.push(intent.revocation.clone());
        tilo_relay::store::save_revocations(identity_root, &relay_revocations)?;
        persist_fleet_revocation(&intent.revocation)?;
        if identity_root.join("home-peers.json").exists() {
            update_home_peer_registry(identity_root, &intent.device, None)?;
        }
        if remote_home_peer(
            identity_root,
            &intent.device,
            &intent.service,
            tilo_relay::transport::HomePeerManagementRequest::Decommission {
                revocation: intent.revocation,
            },
        )
        .is_ok_and(|response| response.decommissioned)
        {
            pending.retain(|queued| queued.device != intent.device);
            save_pending_decommissions(identity_root, &pending)?;
        }
    }
    println!("{}", pending.len());
    Ok(())
}

fn persist_canonical_revocation(
    paths: &tilo_user_identity::IdentityPaths,
    revocation: &tilo_user_identity::RevocationEntry,
) -> anyhow::Result<()> {
    let path = paths
        .user_pubkey
        .parent()
        .ok_or_else(|| anyhow::anyhow!("identity public-key path has no parent"))?
        .join("revocations.bin");
    let mut entries: Vec<tilo_user_identity::RevocationEntry> = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => postcard::from_bytes(&bytes)?,
        Ok(_) => anyhow::bail!("canonical revocation list is too large"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if entries.len() >= 4096 {
        anyhow::bail!("canonical revocation list is full");
    }
    for entry in &entries {
        tilo_user_identity::verify_revocation_entry(entry)?;
        if entry.user_public_key != revocation.user_public_key {
            anyhow::bail!("canonical revocation list crosses identity roots");
        }
    }
    entries.retain(|entry| entry.revoked_device_public_key != revocation.revoked_device_public_key);
    entries.push(revocation.clone());
    entries.sort_by_key(|entry| entry.revoked_device_public_key);
    atomic_file::write_with_mode(&path, &postcard::to_allocvec(&entries)?, 0o600)?;
    Ok(())
}

fn persist_fleet_revocation(
    revocation: &tilo_user_identity::RevocationEntry,
) -> anyhow::Result<()> {
    let config = xdg_paths::xdg::config_home()
        .ok_or_else(|| anyhow::anyhow!("user configuration directory is unavailable"))?;
    let path = config
        .join("tilo/identities/fleets")
        .join(format!("{}.json", hex::encode(revocation.user_public_key)));
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(&path)?;
    if bytes.len() > 4 * 1024 * 1024 {
        anyhow::bail!("device fleet is too large");
    }
    let mut fleet: serde_json::Value = serde_json::from_slice(&bytes)?;
    let revocations = fleet
        .get_mut("revocations")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("device fleet has no revocation list"))?;
    revocations.retain(|entry| {
        entry
            .get("revoked_device_public_key")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|bytes| {
                bytes
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .collect::<Vec<_>>()
                    != revocation
                        .revoked_device_public_key
                        .iter()
                        .map(|byte| u64::from(*byte))
                        .collect::<Vec<_>>()
            })
    });
    revocations.push(serde_json::to_value(revocation)?);
    atomic_file::write_with_mode(&path, &serde_json::to_vec_pretty(&fleet)?, 0o600)?;
    Ok(())
}

fn update_home_peer_registry(
    identity_root: &std::path::Path,
    device: &str,
    new_label: Option<String>,
) -> anyhow::Result<()> {
    let path = identity_root.join("home-peers.json");
    let bytes = std::fs::read(&path)?;
    if bytes.len() > 1024 * 1024 {
        anyhow::bail!("home-peer registry is too large");
    }
    let mut registrations: Vec<tilo_relay::HomePeerRegistration> = serde_json::from_slice(&bytes)?;
    if let Some(label) = new_label {
        let label = label.trim();
        if !tilo_relay::valid_home_peer_label(label) {
            anyhow::bail!("home-peer name must be 1 to 80 bytes");
        }
        let registration = registrations
            .iter_mut()
            .find(|entry| entry.device_node_id == device)
            .ok_or_else(|| anyhow::anyhow!("home peer is not registered"))?;
        registration.label = label.to_string();
    } else {
        registrations.retain(|entry| entry.device_node_id != device);
    }
    atomic_file::write_with_mode(&path, &serde_json::to_vec_pretty(&registrations)?, 0o644)?;
    Ok(())
}

fn cmd_bind_workspace(
    config: &std::path::Path,
    workspace_id: u64,
    pin: bool,
) -> anyhow::Result<()> {
    let mut cfg = load(config)?;
    let added = cfg.add_workspace(workspace_id, pin);
    cfg.save(config)?;
    if added {
        println!("bound workspace {workspace_id} (pin={pin})");
    } else {
        println!("workspace {workspace_id} already bound");
    }
    Ok(())
}

fn cmd_bind_room(config: &std::path::Path, room: &str, pin: bool) -> anyhow::Result<()> {
    let workspace_id = tilo_relay::room_workspace_id(room)
        .with_context(|| format!("invalid room UUID {room:?}"))?;
    let mut cfg = load(config)?;
    let added = cfg.add_workspace(workspace_id, pin);
    cfg.save(config)?;
    println!(
        "{} room {room} as workspace {workspace_id} (pin={pin})",
        if added { "bound" } else { "already bound" }
    );
    Ok(())
}

fn cmd_unbind_workspace(config: &std::path::Path, workspace_id: u64) -> anyhow::Result<()> {
    let mut cfg = load(config)?;
    let removed = cfg.remove_workspace(workspace_id);
    cfg.save(config)?;
    if removed {
        println!("unbound workspace {workspace_id}");
    } else {
        println!("workspace {workspace_id} was not bound");
    }
    Ok(())
}

fn cmd_list_workspaces(config: &std::path::Path) -> anyhow::Result<()> {
    let cfg = load(config)?;
    for ws in &cfg.workspaces {
        println!("{}\tpin={}", ws.id, ws.pin);
    }
    Ok(())
}

fn cmd_add_member(
    config: &std::path::Path,
    workspace_id: u64,
    device_pubkey_hex: &str,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let store = workspace_store(&cfg, workspace_id)?;
    let device = device_key(device_pubkey_hex)?;
    let mut members = store.load_membership()?;
    if !members.contains(&device) {
        members.push(device);
        store.save_membership(&members)?;
    }
    println!("workspace {workspace_id}: admitted {}", hex::encode(device));
    println!("a running tilo-relay reloads membership within one second");
    Ok(())
}

fn cmd_remove_member(
    config: &std::path::Path,
    workspace_id: u64,
    device_pubkey_hex: &str,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let store = workspace_store(&cfg, workspace_id)?;
    let device = device_key(device_pubkey_hex)?;
    let mut members = store.load_membership()?;
    members.retain(|member| member != &device);
    store.save_membership(&members)?;
    println!("workspace {workspace_id}: removed {}", hex::encode(device));
    println!("a running tilo-relay disconnects removed members within one second");
    Ok(())
}

fn cmd_list_members(config: &std::path::Path, workspace_id: u64) -> anyhow::Result<()> {
    let cfg = load(config)?;
    for member in workspace_store(&cfg, workspace_id)?.load_membership()? {
        println!("{}", hex::encode(member));
    }
    Ok(())
}

fn cmd_status(config: &std::path::Path) -> anyhow::Result<()> {
    let cfg = load(config)?;
    println!("tilo-relay-admin {}", tilo_relay::VERSION);
    println!("config:    {}", config.display());
    println!("state:     {}", cfg.state_root.display());
    println!("identity:  {}", cfg.identity_root.display());
    println!("iroh:      {}", cfg.iroh_listen);
    println!("bindings:");
    for ws in &cfg.workspaces {
        println!("  {}\tpin={}", ws.id, ws.pin);
    }
    let workspace_root = cfg.state_root.join("workspaces");
    let mut durable: Vec<_> = std::fs::read_dir(&workspace_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u64>().ok())
        .collect();
    durable.sort_unstable();
    durable.dedup();
    println!("durable workspace shards: {}", durable.len());
    for workspace_id in durable {
        let store = tilo_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, true)?;
        let members = store.load_membership()?.len();
        let retained = store.load_retained()?.len();
        println!("  {workspace_id}\tmembers={members}\tretained={retained}");
    }
    if let Some(metrics) = tilo_relay::store::load_metrics(&cfg.state_root)? {
        println!("metrics:");
        println!("  connections accepted: {}", metrics.conns_accepted);
        println!("  connections authed:   {}", metrics.conns_authed);
        println!("  auth rejected:        {}", metrics.auth_rejected);
        println!("  deltas received:      {}", metrics.deltas_received);
        println!("  deltas fanned out:    {}", metrics.deltas_fanned_out);
        println!("  bytes queued:         {}", metrics.bytes_queued);
    }
    Ok(())
}

/// Sign `entry` with the user root and check the result verifies.
fn sign_revocation_with_root(
    entry: &mut tilo_user_identity::RevocationEntry,
    root: &runtime_identity::SigningIdentity,
) -> anyhow::Result<()> {
    if entry.user_public_key != root.group_public_key() {
        anyhow::bail!("revocation names a different identity than its signer");
    }
    let digest = tilo_user_identity::revocation_signing_digest(entry)?;
    entry.signature = root
        .sign(&digest)
        .map_err(|error| anyhow::anyhow!("the personal identity could not sign: {error}"))?;
    tilo_user_identity::verify_revocation_entry(entry)
        .context("signed revocation does not verify")?;
    Ok(())
}

fn cmd_revoke_device(
    config: &std::path::Path,
    identity_dir: &std::path::Path,
    device_pubkey_hex: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let pk = device_key(device_pubkey_hex)?;
    let reason = match reason {
        "lost" => tilo_user_identity::RevocationReason::Lost,
        "stolen" => tilo_user_identity::RevocationReason::Stolen,
        "retired" => tilo_user_identity::RevocationReason::Retired,
        "compromised" => tilo_user_identity::RevocationReason::Compromised,
        _ => anyhow::bail!("reason must be lost, stolen, retired, or compromised"),
    };
    let root = runtime_identity::load_root_signer(identity_dir)
        .context("cannot revoke: this device must hold the identity root")?;
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.group_public_key(),
        revoked_device_public_key: pk,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason,
        signature: [0; 64],
    };
    sign_revocation_with_root(&mut entry, &root)?;
    let mut entries = tilo_relay::store::load_revocations(&cfg.state_root)?;
    entries.retain(|existing| existing.revoked_device_public_key != pk);
    entries.push(entry);
    tilo_relay::store::save_revocations(&cfg.state_root, &entries)?;
    println!("revoked device {} ({reason:?})", hex::encode(pk));
    println!("a running tilo-relay disconnects the device within one second");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn canonical_revocation_aggregate_is_verified_deduplicated_and_private() {
        let temp = tempfile::tempdir().unwrap();
        let paths = tilo_user_identity::IdentityPaths::under(temp.path());
        std::fs::create_dir_all(paths.user_pubkey.parent().unwrap()).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [72; 32],
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        persist_canonical_revocation(&paths, &entry).unwrap();
        entry.revoked_at_ns = 2;
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        persist_canonical_revocation(&paths, &entry).unwrap();

        let path = paths.user_pubkey.parent().unwrap().join("revocations.bin");
        let stored: Vec<tilo_user_identity::RevocationEntry> =
            postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, vec![entry.clone()]);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let pending = PendingDecommission {
            device: hex::encode(entry.revoked_device_public_key),
            service: hex::encode([73; 32]),
            revocation: entry.clone(),
        };
        save_pending_decommissions(temp.path(), std::slice::from_ref(&pending)).unwrap();
        let after_restart = load_pending_decommissions(temp.path()).unwrap();
        assert_eq!(after_restart.len(), 1);
        assert_eq!(after_restart[0].device, pending.device);
        assert_eq!(after_restart[0].revocation, entry);
    }
}
