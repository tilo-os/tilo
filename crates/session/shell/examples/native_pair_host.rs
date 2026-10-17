//! Private governed host for the two-native-desktop acceptance fixture.
//! Only marked throwaway profiles are provisioned. Invitations and identity
//! custody stay in that private directory; stdout contains stage names only.

use ed25519_dalek::SigningKey;
use getrandom::SysRng;
use rand_core::UnwrapErr;
use std::{path::Path, sync::Arc, time::Duration};
use tilo_agent_participant::{AppCatalogue, ModelChoice};
use tilo_identity_host::{
    device_fleet::DeviceFleetStore, enrollment::EnrollmentRole, provisioning,
};
use tilo_relay::{
    config::RelayConfig,
    daemon::{Relay, RelayIdentity},
    rooms::{GrantAppAuthorshipRequest, IssueInvitationRequest, RemoveMemberRequest},
};
use tilo_user_identity::{DeviceCert, IdentityPaths, RevocationList};
use tokio::io::{AsyncBufReadExt as _, BufReader};

fn provision(profile: &Path) -> Result<DeviceCert, Box<dyn std::error::Error>> {
    let paths = IdentityPaths::under(profile.join("identity"));
    provisioning::ensure_throwaway_test_identity(
        &profile.join("config/tilo/identity"),
        &paths,
        provisioning::IdentityStorage::Files,
    )?;
    Ok(tilo_user_identity::persist::read_device_cert(
        &paths.device_cert,
    )?)
}

fn enroll_host(
    profile: &Path,
    owner: &DeviceCert,
) -> Result<RelayIdentity, Box<dyn std::error::Error>> {
    let fleet = DeviceFleetStore::at(
        profile.join("config/tilo/identity"),
        IdentityPaths::under(profile.join("identity")),
    );
    let now = tilo_clock::unix_now_ns();
    let expires = now.checked_add(600_000_000_000).ok_or("expiry overflow")?;
    let identity = hex::encode(owner.user_public_key);
    let role = EnrollmentRole::HomePeer;
    let token = fleet.mint_enrollment(&identity, role.preset(), now, expires)?;
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = fleet.complete_enrollment(
        &identity,
        &token,
        role,
        key.verifying_key().to_bytes(),
        "Private native pair host",
        now,
    )?;
    Ok(RelayIdentity {
        user_public_key: owner.user_public_key,
        device_signing_key: key,
        device_cert: cert,
        revocations: RevocationList::new(),
    })
}

async fn invitations(
    relay: &Relay,
    identities: &[DeviceCert; 2],
    host: &DeviceCert,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let rooms = relay.rooms().ok_or("host room service absent")?;
    let room = rooms.create("Private native pair").await?;
    rooms
        .grant_app_authorship(
            &room.room,
            host,
            GrantAppAuthorshipRequest {
                request_id: [4; 32],
                app: "so.tilo.chat".into(),
            },
        )
        .await?;
    rooms
        .prepare_application(&room.room, host, &chat_catalogue(), "so.tilo.chat", "")
        .await?;
    let expires = tilo_clock::unix_now_ns()
        .checked_add(600_000_000_000)
        .ok_or("expiry overflow")?;
    let mut invitations = Vec::new();
    for (index, identity) in identities.iter().enumerate() {
        let request_id = [u8::try_from(index)?
            .checked_add(1)
            .ok_or("request ID overflow")?; 32];
        let issued = rooms
            .issue(
                &room.room,
                host,
                IssueInvitationRequest {
                    request_id,
                    role: tilo_peer_state::join::MemberRole::Editor,
                    recipient_user_public_key: Some(identity.user_public_key),
                    invitee_name: Some(format!("Fixture peer {index}")),
                    inviter_name: None,
                    expires_at_ns: expires,
                },
            )
            .await?;
        invitations.push(tilo_peer_state::join::encode_invitation_text(
            &issued.signed_invitation,
        ));
    }
    Ok(
        serde_json::json!({"room": room.room, "host": relay.node_addr(), "invitations": invitations,
            "users": identities.iter().map(|cert| hex::encode(cert.user_public_key)).collect::<Vec<_>>(),
            "presence_ids": identities.iter().map(|cert| format!("peer@{}", neighbour_wire::peer_handle(&cert.device_public_key))).collect::<Vec<_>>()}),
    )
}

fn chat_catalogue() -> AppCatalogue {
    AppCatalogue::new(
        AppCatalogue::native()
            .iter()
            .filter(|app| app.app_id == "so.tilo.chat")
            .collect(),
    )
}

fn enroll_member(root: &Path) -> Result<DeviceCert, Box<dyn std::error::Error>> {
    let alice = root.join("alice");
    let bob = root.join("bob");
    let identity_dir = alice.join("config/tilo/identity");
    if !provisioning::is_throwaway_test_identity(&identity_dir)
        || runtime_identity::resolve_active_identity(&bob.join("config/tilo/identity")).is_some()
    {
        return Err("member installation requires marked disposable owner and empty member".into());
    }
    let paths = IdentityPaths::under(alice.join("identity"));
    let target = bob.join("joined");
    let cert = provisioning::enroll_from_local_identity(
        &provisioning::JoinedDeviceLayout {
            target_root: &target,
            username: "bob",
            local_identity_dir: &identity_dir,
            local_system: &paths,
            local_home_peers: &alice.join("identity/home-peers.json"),
            local_rooms: &alice.join("state/tilo/personal-rooms.json"),
        },
        "Same-person fixture",
        "Fixture second device",
        tilo_clock::unix_now_ns(),
    )?;
    // Authorization artifacts retain their installed paths. Run the member
    // against this layout rather than relocating its certificate authority.
    let member = IdentityPaths::under(target.join("var/lib/tilo"));
    if member.user_privkey.exists() || cert.capabilities.can_admit_devices {
        return Err("member must not receive personal signing custody".into());
    }
    Ok(cert)
}

async fn serve(root: &Path, same_person: bool) -> Result<(), Box<dyn std::error::Error>> {
    let first = provision(&root.join("alice"))?;
    let second = if same_person {
        enroll_member(root)?
    } else {
        provision(&root.join("bob"))?
    };
    let identities = [first, second];
    if (identities[0].user_public_key == identities[1].user_public_key) != same_person {
        return Err("native pair person identities do not match the selected mode".into());
    }
    if identities[0].device_public_key == identities[1].device_public_key {
        return Err("native fixture requires independent device keys".into());
    }
    let host = enroll_host(&root.join("alice"), &identities[0])?;
    let cert = host.device_cert.clone();
    let (relay, _agents) = tilo_relay::agents::start_with_relay_mode(
        RelayConfig {
            state_root: root.join("host"),
            iroh_listen: "127.0.0.1:0".parse()?,
            ..RelayConfig::default()
        },
        host,
        tilo_relay::agents::AgentOptions {
            state_dir: root.join("agents"),
            model: ModelChoice::Scripted,
            catalogue: Some(Arc::new(chat_catalogue)),
        },
        iroh::RelayMode::Disabled,
    )
    .await?;
    let web = relay
        .serve_web(tilo_relay::web::WebConfig {
            listen: "127.0.0.1:0".parse()?,
            tls: tilo_relay::web::TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: None,
            },
            allowed_origins: Vec::new(),
            plain_listen: None,
            public_origin: None,
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
            country: None,
        })
        .await?;
    let bootstrap = invitations(&relay, &identities, &cert).await?;
    if !tilo_files::native::private::create_atomic(
        &root.join("bootstrap.json"),
        &serde_json::to_vec(&bootstrap)?,
    )? {
        return Err("bootstrap output already exists".into());
    }
    println!("ready");
    let workspace_room = bootstrap
        .get("room")
        .and_then(serde_json::Value::as_str)
        .ok_or("room missing")?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(command) =
        tokio::time::timeout(Duration::from_secs(600), lines.next_line()).await??
    {
        match command.as_str() {
            "revoke" => {
                relay
                    .rooms()
                    .ok_or("rooms absent")?
                    .remove_member(
                        workspace_room,
                        &cert,
                        RemoveMemberRequest {
                            request_id: [3; 32],
                            user_public_key: identities[1].user_public_key,
                        },
                    )
                    .await?;
                println!("revoked");
            }
            "stop" => break,
            _ => return Err("unknown fixture stage".into()),
        }
    }
    drop(web);
    relay.shutdown().await;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    if let Some(code) = tilo_agent_participant::worker::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--journey")) {
        let status = std::process::Command::new("python3")
            .arg("-B")
            .arg("scripts/headless-examples/native-pair-journey.py")
            .args(std::env::args_os().skip(2))
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err("native pair acceptance failed".into())
        };
    }
    let root = std::env::args_os()
        .nth(1)
        .ok_or("private scratch root required")?;
    let root = Path::new(&root).canonicalize()?;
    if !tilo_files::native::private::is_owner_only(&std::fs::metadata(&root)?) {
        return Err("fixture root must already be private".into());
    }
    if std::env::var_os("TILO_RELAY_PHASE_DIAGNOSTICS").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|error| error.to_string())?;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(serve(
            &root,
            std::env::args_os().nth(2).as_deref() == Some(std::ffi::OsStr::new("--same-person")),
        ))
}
