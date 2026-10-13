//! The room host under load: a relay governing public rooms, serving
//! browsers on plain loopback HTTP, with a `/join` that pre-admits each new
//! guest the way the room service does (a root-signed `PreAdmission` for a
//! one-time token, answered with the single-use invitation).
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
use tilo_relay_protocol::rooms::{PreAdmission, RoomRole, RoomsReply, RoomsRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tokio::io::AsyncBufReadExt as _;

use crate::http::Plain;
use crate::{HostArgs, Started, StartedRoom};

/// How long a pre-admission stays redeemable, as the service issues them.
const PRE_ADMISSION_NS: u64 = 10 * 60 * 1_000_000_000;
/// The room service's answer to a visitor past its cap: 503, a wait, and
/// this message.
const FULL: &str = "the room is full";
const FULL_RETRY_SECS: u64 = 60;

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

struct Host {
    root: SigningKey,
    owner: HttpsLink,
    origin: String,
    seats: Mutex<Seats>,
    /// Guests `/join` admits; none leave during a run.
    max_guests: Option<usize>,
    admitted: AtomicUsize,
}

impl Host {
    async fn pre_admit(&self, room: &StartedRoom) -> anyhow::Result<serde_json::Value> {
        let token = hex::encode(rand::random::<[u8; 32]>());
        let mut grant = PreAdmission {
            room: room.room.clone(),
            role: RoomRole::Editor,
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
            let mut seats = self
                .seats
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = seats.assign().context("no rooms")?;
            (
                seats.desktops.get(index).cloned().context("room")?,
                seats.chat.clone(),
            )
        };
        let mut rooms = vec![self.pre_admit(&desktop).await?];
        if let Some(chat) = &chat {
            rooms.push(self.pre_admit(chat).await?);
        }
        Ok(serde_json::json!({ "origin": self.origin, "rooms": rooms }))
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
                        let _ = request.into_body().collect().await;
                        let mut response =
                            hyper::Response::builder().header("content-type", "application/json");
                        let (status, body) = if host.seat() {
                            match host.join().await {
                                Ok(reply) => (200, reply.to_string()),
                                Err(error) => (
                                    503,
                                    serde_json::json!({ "error": error.to_string() }).to_string(),
                                ),
                            }
                        } else {
                            response = response.header("retry-after", FULL_RETRY_SECS);
                            (503, serde_json::json!({ "error": FULL }).to_string())
                        };
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
    let relay = Relay::start(
        tilo_relay::RelayConfig {
            state_root: state.path().to_path_buf(),
            // Sized as a public room's host is: every guest is one grant.
            max_room_grants: Some(16 * 1024),
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
        .serve_web(WebConfig {
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
                max_sessions: 4096,
                max_handshakes: 1024,
                max_live_grants: 16 * 1024,
            },
        })
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
        plain,
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
    let join = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let started = Started {
        origin: origin.clone(),
        join: join.local_addr()?.to_string(),
        desktops: desktops.clone(),
        chat: chat.clone(),
    };
    let taken = vec![0; desktops.len()];
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
    });
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
