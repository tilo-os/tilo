//! The room host under load: a relay governing public rooms, serving
//! browsers on plain loopback HTTP, with a `/join` that pre-admits each new
//! guest the way the room service does (a root-signed `PreAdmission` for a
//! one-time token, answered with the single-use invitation).
//!
//! It also answers `/spectate` as the room service's persistent room does:
//! the first desktop room has one Viewer member, the spectator person, and
//! every watching page's in-memory device key is certified as that person,
//! under its own cap (`--max-spectators`).
//!
//! It runs as its own process so its CPU and memory can be read alone.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use base64::Engine as _;
use bytes::Bytes;
use ed25519_dalek::{Signer as _, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use http_body_util::{BodyExt as _, Full};
use sha2::{Digest as _, Sha256};
use tilo_browser_peer::{HttpClient, HttpsLink, carrier};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay_protocol::rooms::{JoinReply, PreAdmission, RoomRole, RoomsReply, RoomsRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tokio::io::AsyncBufReadExt as _;

use crate::http::Plain;
use crate::{HostArgs, Started, StartedRoom};
use tilo_lock::MutexRecover as _;

/// How long a pre-admission stays redeemable, as the service issues them.
const PRE_ADMISSION_NS: u64 = 10 * 60 * 1_000_000_000;
/// The room service's answer to a visitor past its cap: 503, a wait, and
/// this message.
const FULL: &str = "the room is full";
const FULL_RETRY_SECS: u64 = 60;
/// The room service's answer to a spectator past the spectator cap.
const WATCHING_FULL: &str = "too many people are watching";
/// How long a spectator's certificate is valid, as the persistent room
/// issues them.
const SPECTATOR_CERT_NS: u64 = 24 * 60 * 60 * 1_000_000_000;
/// The member-list name of the spectator person.
const SPECTATOR_NAME: &str = "Spectators";

fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> anyhow::Result<(SigningKey, DeviceCert)> {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).context("sign a device certificate")?;
    Ok((key, cert))
}

/// Who sits in which room, for least-full assignment.
struct Seats {
    desktops: Vec<StartedRoom>,
    chat: Option<StartedRoom>,
    taken: Vec<usize>,
    shard: usize,
}

impl Seats {
    /// The least-full desktop room, or the first one with room in shard
    /// order: fill one room to `shard` before opening the next, so a quiet
    /// public room still feels like one busy room.
    fn assign(&mut self) -> Option<usize> {
        let index = self
            .taken
            .iter()
            .position(|taken| *taken < self.shard)
            .or_else(|| {
                (0..self.taken.len())
                    .min_by_key(|index| self.taken.get(*index).copied().unwrap_or(usize::MAX))
            })?;
        if let Some(taken) = self.taken.get_mut(index) {
            *taken = taken.saturating_add(1);
        }
        Some(index)
    }
}

/// The spectator person: a Viewer of the first desktop room whose root
/// certifies each watching page's device.
struct Spectator {
    root: SigningKey,
    room: StartedRoom,
    max: Option<usize>,
    watching: AtomicUsize,
}

/// A certificate for one watching page's device: the spectator person, no
/// capabilities, expiring, as the persistent room signs them.
fn spectator_cert(root: &SigningKey, device: [u8; 32]) -> anyhow::Result<DeviceCert> {
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device,
        device_label: "Watching visitor".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(SPECTATOR_CERT_NS)),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).context("sign a spectator certificate")?;
    Ok(cert)
}

struct Host {
    root: SigningKey,
    owner: HttpsLink,
    origin: String,
    seats: Mutex<Seats>,
    /// Guests `/join` admits; none leave during a run.
    max_guests: Option<usize>,
    admitted: AtomicUsize,
    spectator: Spectator,
}

impl Host {
    async fn pre_admit(
        &self,
        room: &StartedRoom,
        role: RoomRole,
    ) -> anyhow::Result<serde_json::Value> {
        let token = hex::encode(rand::random::<[u8; 32]>());
        let mut grant = PreAdmission {
            room: room.room.clone(),
            role,
            token_sha256: Sha256::digest(token.as_bytes()).into(),
            label: "room guest".into(),
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(PRE_ADMISSION_NS),
            owner_user_public_key: self.root.verifying_key().to_bytes(),
            signature: [0; 64],
        };
        grant.signature = self
            .root
            .sign(&grant.digest().context("digest")?)
            .to_bytes();
        let reply = carrier::rooms(
            self.owner.client().as_ref(),
            self.owner.session(),
            &RoomsRequest::PreAdmit {
                request_id: rand::random(),
                grant,
                inviter_name: Some("Tilo".into()),
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("pre-admit: {error}"))?;
        let RoomsReply::Invitation {
            signed_invitation, ..
        } = reply
        else {
            bail!("pre-admit answered {reply:?}");
        };
        Ok(serde_json::json!({
            "room": room.room,
            "workspaceId": room.workspace_id,
            "invitation": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signed_invitation),
            "admissionToken": token,
        }))
    }

    /// Takes a seat under `--max-guests`, or `false` when the room is full.
    fn seat(&self) -> bool {
        self.admitted
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |admitted| {
                self.max_guests
                    .is_none_or(|max| admitted < max)
                    .then_some(admitted + 1)
            })
            .is_ok()
    }

    async fn join(&self) -> anyhow::Result<serde_json::Value> {
        let (desktop, chat) = {
            let mut seats = self.seats.lock_recover();
            let index = seats.assign().context("no rooms")?;
            (
                seats.desktops.get(index).cloned().context("room")?,
                seats.chat.clone(),
            )
        };
        let mut rooms = vec![self.pre_admit(&desktop, RoomRole::Editor).await?];
        if let Some(chat) = &chat {
            rooms.push(self.pre_admit(chat, RoomRole::Editor).await?);
        }
        Ok(serde_json::json!({ "origin": self.origin, "rooms": rooms }))
    }

    /// Make the spectator person a Viewer member of its room, through the
    /// same pre-admission and join a guest's page takes.
    async fn admit_spectator_person(&self, relay: &dyn HttpClient) -> anyhow::Result<()> {
        let admission = self
            .pre_admit(&self.spectator.room, RoomRole::Viewer)
            .await?;
        let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(
                admission
                    .get("invitation")
                    .and_then(serde_json::Value::as_str)
                    .context("invitation")?,
            )
            .context("invitation")?;
        let token = admission
            .get("admissionToken")
            .and_then(serde_json::Value::as_str)
            .context("admission token")?;
        let device = SigningKey::generate(&mut UnwrapErr(SysRng));
        let cert = spectator_cert(&self.spectator.root, device.verifying_key().to_bytes())?;
        let reply = tilo_browser_peer::invite::ask_to_join_pre_admitted(
            relay,
            &invitation,
            &cert,
            &device,
            rand::random(),
            SPECTATOR_NAME,
            token.as_bytes(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("the spectator person's join: {error}"))?;
        if !matches!(reply, JoinReply::Admitted { .. }) {
            bail!("the spectator person's join was answered {reply:?}");
        }
        Ok(())
    }

    /// `POST /spectate`: certify a watching page's device as the spectator
    /// person, or (`"member": true`) pre-admit a Viewer of its own; `None`
    /// past the spectator cap.
    async fn spectate(&self, body: &[u8]) -> anyhow::Result<Option<serde_json::Value>> {
        let request: serde_json::Value = serde_json::from_slice(body).context("request")?;
        let member = request
            .get("member")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let device: Option<[u8; 32]> = request
            .get("devicePublicKey")
            .and_then(serde_json::Value::as_str)
            .and_then(|text| hex::decode(text).ok())
            .and_then(|bytes| bytes.try_into().ok());
        if !member && device.is_none() {
            bail!("devicePublicKey is 64 hex characters");
        }
        let seated = self
            .spectator
            .watching
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |watching| {
                self.spectator
                    .max
                    .is_none_or(|max| watching < max)
                    .then_some(watching + 1)
            })
            .is_ok();
        if !seated {
            return Ok(None);
        }
        let Some(device) = device.filter(|_| !member) else {
            let admission = self
                .pre_admit(&self.spectator.room, RoomRole::Viewer)
                .await?;
            return Ok(Some(
                serde_json::json!({ "origin": self.origin, "rooms": [admission] }),
            ));
        };
        let certificate = spectator_cert(&self.spectator.root, device)?;
        Ok(Some(serde_json::json!({
            "certificate": certificate,
            "origin": self.origin,
            "room": self.spectator.room.room,
            "workspaceId": self.spectator.room.workspace_id,
        })))
    }

    /// One `/join` or `/spectate` request's status, `retry-after` and body.
    async fn answer(&self, path: &str, body: &[u8]) -> (u16, Option<u64>, String) {
        let error = |message: String| serde_json::json!({ "error": message }).to_string();
        if path == "/spectate" {
            return match self.spectate(body).await {
                Ok(Some(reply)) => (200, None, reply.to_string()),
                Ok(None) => (503, Some(FULL_RETRY_SECS), error(WATCHING_FULL.into())),
                Err(problem) => (400, None, error(format!("{problem:#}"))),
            };
        }
        if !self.seat() {
            return (503, Some(FULL_RETRY_SECS), error(FULL.into()));
        }
        match self.join().await {
            Ok(reply) => (200, None, reply.to_string()),
            Err(problem) => (503, None, error(problem.to_string())),
        }
    }
}

async fn create_room(owner: &HttpsLink, name: &str) -> anyhow::Result<StartedRoom> {
    let created = carrier::rooms(
        owner.client().as_ref(),
        owner.session(),
        &RoomsRequest::Create { name: name.into() },
    )
    .await
    .map_err(|error| anyhow::anyhow!("create a room: {error}"))?;
    let RoomsReply::Room(created) = created else {
        bail!("create answered {created:?}");
    };
    Ok(StartedRoom {
        room: created.room,
        workspace_id: created.workspace_id,
    })
}

/// Have `room`'s governor (`host`) write the chat record guests write, as a
/// persistent room seeds its chat, so it holds the record from the start.
async fn seed_chat(
    rooms: &tilo_relay::rooms::Rooms,
    room: &StartedRoom,
    host: &DeviceCert,
) -> anyhow::Result<()> {
    rooms
        .author_room_record(
            &room.room,
            host,
            ("so.tilo.chat", crate::guest::CHAT_RECORD),
            |doc| {
                automerge::transaction::Transactable::put(doc, automerge::ROOT, "seeded", true)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("seed the chat: {error}"))
}

async fn serve_join(host: Arc<Host>, listener: tokio::net::TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let host = host.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    let host = host.clone();
                    async move {
                        let path = request.uri().path().to_owned();
                        let body = request
                            .into_body()
                            .collect()
                            .await
                            .map(http_body_util::Collected::to_bytes)
                            .unwrap_or_default();
                        let (status, retry_after, body) = host.answer(&path, &body).await;
                        let mut response =
                            hyper::Response::builder().header("content-type", "application/json");
                        if let Some(secs) = retry_after {
                            response = response.header("retry-after", secs);
                        }
                        response.status(status).body(Full::new(Bytes::from(body)))
                    }
                },
            );
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await;
        });
    }
}

/// Web sessions the relay allows, sized from the caps as the room service
/// sizes its persistent room's relay (every guest and spectator holds one,
/// plus 64), with room for 4032 guests and 4096 spectators when uncapped.
fn web_sessions(args: &HostArgs) -> usize {
    args.max_guests
        .unwrap_or(4032)
        .saturating_add(args.max_spectators.unwrap_or(4096))
        .saturating_add(64)
}

/// The browser routes on loopback, with the room service's limits for
/// `sessions` web sessions.
fn web_config(sessions: usize) -> anyhow::Result<WebConfig> {
    Ok(WebConfig {
        listen: "127.0.0.1:0".parse()?,
        tls: TlsSource::Development {
            hosts: vec!["localhost".into(), "127.0.0.1".into()],
            store: None,
        },
        allowed_origins: Vec::new(),
        plain_listen: Some("127.0.0.1:0".parse()?),
        public_origin: None,
        live_origin: None,
        limits: tilo_relay::web::WebLimits {
            max_sessions: sessions,
            max_handshakes: 1024,
            max_live_grants: sessions.saturating_mul(4),
        },
    })
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay lives until stdin closes, then shuts down by value"
)]
pub async fn run(args: HostArgs) -> anyhow::Result<()> {
    let state = tempfile::tempdir().context("state directory")?;
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (host_key, host_cert) = enrol(
        &root,
        "room-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    )?;
    let (owner_key, owner_cert) = enrol(&root, "room-owner", DeviceCapabilities::member())?;
    let sessions = web_sessions(&args);
    let relay = Relay::start(
        tilo_relay::RelayConfig {
            state_root: state.path().to_path_buf(),
            max_room_grants: Some(sessions.saturating_mul(16)),
            ..Default::default()
        },
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host_key,
            device_cert: host_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .context("start the relay")?;
    let web = relay
        .serve_web(web_config(sessions)?)
        .await
        .context("serve the browser routes")?;
    let origin = web.plain_origin.clone().context("a plain origin")?;
    let port: u16 = origin
        .rsplit(':')
        .next()
        .context("a port")?
        .parse()
        .context("a port")?;
    let plain: Arc<dyn HttpClient> = Arc::new(Plain::new(SocketAddr::from(([127, 0, 0, 1], port))));
    let owner = HttpsLink::connect(
        plain.clone(),
        &owner_cert,
        host_cert.device_public_key,
        &RevocationList::new(),
        &owner_key,
    )
    .await
    .map_err(|error| anyhow::anyhow!("the owner's page: {error}"))?;
    let mut desktops = Vec::new();
    for index in 0..args.rooms.max(1) {
        desktops.push(create_room(&owner, &format!("Desktop {index}")).await?);
    }
    let chat = if args.chat_room {
        Some(create_room(&owner, "Chat").await?)
    } else {
        None
    };
    if args.seeded_chat {
        let rooms = relay.rooms().context("the relay governs rooms")?;
        for room in desktops.iter().chain(&chat) {
            seed_chat(rooms, room, &host_cert).await?;
        }
    }
    let join = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let started = Started {
        origin: origin.clone(),
        join: join.local_addr()?.to_string(),
        desktops: desktops.clone(),
        chat: chat.clone(),
    };
    let taken = vec![0; desktops.len()];
    let spectator = Spectator {
        root: SigningKey::generate(&mut UnwrapErr(SysRng)),
        room: desktops.first().cloned().context("a desktop room")?,
        max: args.max_spectators,
        watching: AtomicUsize::new(0),
    };
    let host = Arc::new(Host {
        root,
        owner,
        origin,
        seats: Mutex::new(Seats {
            desktops,
            chat,
            taken,
            shard: args.shard.max(1),
        }),
        max_guests: args.max_guests,
        admitted: AtomicUsize::new(0),
        spectator,
    });
    host.admit_spectator_person(plain.as_ref()).await?;
    tokio::spawn(serve_join(host, join));
    println!("{}", serde_json::to_string(&started)?);
    // Run until the driver closes our stdin (or dies).
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(_)) = lines.next_line().await {}
    drop(web);
    relay.shutdown().await;
    drop(state);
    Ok(())
}
