//! One browser guest: `/join`, redeem each invitation with its one-time
//! token, hold a certified session subscribed to the rooms, move a cursor
//! on the desktop room's live plane and post to the chat.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use base64::Engine as _;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde::Deserialize;
use tilo_browser_peer::invite::{ask_to_join_pre_admitted, guest_certificate};
use tilo_browser_peer::{DocumentRoute, Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle};
use tilo_relay_protocol::rooms::JoinReply;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::DriveArgs;
use crate::http::Plain;
use crate::live::Live;
use crate::stats::Stats;

/// The relay's keepalive cadence for an idle page's presence.
const KEEPALIVE: Duration = Duration::from_secs(3);
/// Mean length of a burst of cursor motion, and of a pause.
const PHASE_MS: f64 = 4000.0;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JoinedRoom {
    room: String,
    workspace_id: u64,
    invitation: String,
    admission_token: String,
}

#[derive(Deserialize)]
struct Joined {
    rooms: Vec<JoinedRoom>,
}

pub struct Shared {
    pub args: DriveArgs,
    pub relay: Arc<Plain>,
    pub service: Plain,
    pub host_cert: DeviceCert,
    pub stats: Arc<Stats>,
    pub started: Instant,
    pub stop: tokio::sync::watch::Receiver<bool>,
}

fn exponential(mean_ms: f64) -> Duration {
    let uniform: f64 = rand::random::<f64>().max(1e-9);
    Duration::from_secs_f64((-uniform.ln() * mean_ms / 1000.0).min(3600.0))
}

/// What the room service tells a visitor past its cap.
const FULL: &str = "the room is full";

fn now_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

async fn admit(
    shared: &Shared,
    name: &str,
) -> Result<(SigningKey, DeviceCert, Vec<JoinedRoom>), String> {
    let (status, body) = shared
        .service
        .post_with(
            "/join",
            &[("content-type", "application/json")],
            serde_json::json!({ "userPublicKey": "", "displayName": name })
                .to_string()
                .into_bytes(),
        )
        .await?;
    if status == 503
        && serde_json::from_slice::<serde_json::Value>(&body)
            .is_ok_and(|reply| reply.get("error").and_then(|e| e.as_str()) == Some(FULL))
    {
        return Err(FULL.into());
    }
    if status != 200 {
        return Err(format!("/join: status {status}"));
    }
    let joined: Joined = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = guest_certificate(
        &root,
        key.verifying_key().to_bytes(),
        name,
        tilo_clock::unix_now_ns(),
    )
    .await
    .map_err(|error| error.to_string())?;
    for room in &joined.rooms {
        let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&room.invitation)
            .map_err(|error| error.to_string())?;
        let reply = ask_to_join_pre_admitted(
            shared.relay.as_ref(),
            &invitation,
            &cert,
            &key,
            rand::random(),
            name,
            room.admission_token.as_bytes(),
        )
        .await
        .map_err(|error| format!("ask: {error}"))?;
        if !matches!(reply, JoinReply::Admitted { .. }) {
            return Err(format!("ask answered {reply:?}"));
        }
    }
    Ok((key, cert, joined.rooms))
}

/// Run one guest until the run stops.
#[allow(
    clippy::too_many_lines,
    reason = "one guest's life reads top to bottom: join, then the activity loop"
)]
pub async fn run(shared: Arc<Shared>, index: usize) {
    let stats = shared.stats.clone();
    let begun = Instant::now();
    let name = format!("Guest {index}");
    let (key, cert, rooms) = match admit(&shared, &name).await {
        Ok(admitted) => admitted,
        Err(error) if error == FULL => {
            stats.full(begun.elapsed().as_secs_f64() * 1000.0);
            return;
        }
        Err(error) => {
            stats.fail(format!("join: {error}"));
            return;
        }
    };
    let admitted_ms = begun.elapsed().as_secs_f64() * 1000.0;
    let peer = PeerHandle::start(
        PeerConfig {
            host: Host::Https(shared.relay.clone()),
            cert,
            host_cert: shared.host_cert.clone(),
            revocations: RevocationList::new(),
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    );
    for room in &rooms {
        if let Err(error) = peer.join(room.workspace_id) {
            stats.fail(format!("subscribe: {error}"));
            return;
        }
    }
    let mut ready = std::collections::HashSet::new();
    let mut stop = shared.stop.clone();
    let deadline = begun + Duration::from_secs(shared.args.join_timeout);
    while ready.len() < rooms.len() {
        for event in peer.drain() {
            if let PeerEvent::Ready { workspace_id } = event {
                ready.insert(workspace_id);
            }
        }
        if Instant::now() > deadline || *stop.borrow() {
            stats.fail("join: no Ready before the timeout");
            peer.close();
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stats.join(admitted_ms, begun.elapsed().as_secs_f64() * 1000.0);
    let joined_at = now_ms(shared.started);

    let desktop = rooms.first().map(|room| room.workspace_id);
    let chat = rooms.last().map(|room| DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.room.clone(),
        record: "general".into(),
    });

    // The cursor: a fallback live session on the desktop room.
    let mut live = None;
    if shared.args.cursor_hz > 0.0
        && let Some(desktop) = desktop
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let asked = peer.live_grant_then(
            desktop,
            Box::new(move |grant| {
                let _ = sender.send(grant);
            }),
        );
        match (asked, receiver.await) {
            (Ok(()), Ok(Ok(grant))) => {
                match Live::open(shared.relay.clone(), &grant.token, stats.clone()).await {
                    Ok(opened) => {
                        stats.live_open.fetch_add(1, Ordering::Relaxed);
                        live = Some(opened);
                    }
                    Err(error) => stats.fail(error),
                }
            }
            (Err(error), _) | (_, Ok(Err(error))) => stats.fail(format!("live grant: {error}")),
            (_, Err(_)) => stats.fail("live grant: dropped"),
        }
    }

    let tick = if shared.args.cursor_hz > 0.0 {
        Duration::from_secs_f64(1.0 / shared.args.cursor_hz)
    } else {
        Duration::from_millis(100)
    };
    let mut active = rand::random::<f64>() < shared.args.active_fraction;
    let mut phase_ends = Instant::now() + exponential(PHASE_MS);
    let mut last_presence = Instant::now()
        .checked_sub(KEEPALIVE)
        .unwrap_or_else(Instant::now);
    let mut next_chat = (shared.args.chat_every > 0.0)
        .then(|| Instant::now() + exponential(shared.args.chat_every * 1000.0));
    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seen_failures = 0;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = stop.changed() => {}
        }
        if *stop.borrow() {
            break;
        }
        let now = Instant::now();
        if now >= phase_ends {
            // Visitors move in bursts and pause; the mix keeps the share
            // of moving cursors at `active_fraction` on average.
            active = rand::random::<f64>() < shared.args.active_fraction;
            phase_ends = now + exponential(PHASE_MS);
        }
        if let Some(live) = live.as_mut()
            && (active || now.duration_since(last_presence) >= KEEPALIVE)
        {
            last_presence = now;
            match live.presence().await {
                Ok(()) => {
                    stats.presence_tx.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    stats.presence_refused.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if let (Some(at), Some(route)) = (next_chat, chat.as_ref())
            && now >= at
        {
            let mut message = now_ms(shared.started).to_be_bytes().to_vec();
            message.extend((0..shared.args.chat_bytes).map(|_| rand::random::<u8>()));
            if peer
                .publish(route.clone(), &DocumentMutation::Incremental(message))
                .is_ok()
            {
                stats.chat_tx.fetch_add(1, Ordering::Relaxed);
            }
            next_chat = Some(now + exponential(shared.args.chat_every * 1000.0));
        }
        for event in peer.drain() {
            match event {
                PeerEvent::Document(document) => match document.mutation() {
                    Some(DocumentMutation::Incremental(bytes)) if bytes.len() >= 8 => {
                        let mut at = [0u8; 8];
                        at.copy_from_slice(bytes.get(..8).unwrap_or(&[0; 8]));
                        let sent = u64::from_be_bytes(at);
                        if sent < joined_at {
                            stats.chat_history();
                        } else {
                            let latency = now_ms(shared.started).saturating_sub(sent);
                            stats.chat(sent, latency as f64);
                        }
                    }
                    other => stats.fail(format!(
                        "document without a readable change (sealed: {}, mutation: {})",
                        document.decrypted_payload.is_some(),
                        other.is_some()
                    )),
                },
                PeerEvent::DocumentRejected(reason) => {
                    stats.fail(format!("document rejected: {reason}"));
                }
                PeerEvent::Committed { .. } => {
                    stats.committed.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
        let failures = peer.status().failures.len();
        if failures > seen_failures {
            stats
                .session_failures
                .fetch_add((failures - seen_failures) as u64, Ordering::Relaxed);
            for failure in peer.status().failures.iter().skip(seen_failures) {
                stats.fail(format!("session: {} {}", failure.code, failure.message));
            }
        }
        seen_failures = failures;
    }
    peer.close();
}
