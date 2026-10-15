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
pub struct JoinedRoom {
    room: String,
    pub workspace_id: u64,
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
    /// What the spectators measured, apart from the guests.
    pub spectator_stats: Arc<Stats>,
    pub started: Instant,
    pub stop: tokio::sync::watch::Receiver<bool>,
}

/// The chat record every guest writes, in the last room it joined.
pub const CHAT_RECORD: &str = "general";
/// The field of the chat record a guest's message sets.
const MESSAGE: &str = "message";

/// One chat message as a guest's page writes it: an Automerge change of the
/// chat record setting [`MESSAGE`] to the time it was sent (milliseconds
/// since the run started, big-endian) and `padding` bytes, by an actor of
/// its own, so it applies to the record whatever else the record holds.
fn chat_message(sent_ms: u64, padding: usize) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    use collab_types::CommitNamed as _;
    let mut message = sent_ms.to_be_bytes().to_vec();
    message.extend((0..padding).map(|_| rand::random::<u8>()));
    let mut change = automerge::AutoCommit::new();
    if change
        .put(
            automerge::ROOT,
            MESSAGE,
            automerge::ScalarValue::Bytes(message),
        )
        .is_err()
    {
        return Vec::new();
    }
    change.commit_edit("chat", MESSAGE);
    change.save_incremental()
}

/// When the chat message `change` was sent, as [`chat_message`] wrote it.
fn chat_sent_ms(change: &[u8]) -> Option<u64> {
    use automerge::ReadDoc as _;
    let mut document = automerge::AutoCommit::new();
    tilo_automerge::ApplyIncoming::load_incoming(&mut document, change).ok()?;
    let (automerge::Value::Scalar(value), _) = document.get(automerge::ROOT, MESSAGE).ok()?? else {
        return None;
    };
    let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    Some(u64::from_be_bytes(bytes.get(..8)?.try_into().ok()?))
}

fn exponential(mean_ms: f64) -> Duration {
    let uniform: f64 = rand::random::<f64>().max(1e-9);
    Duration::from_secs_f64((-uniform.ln() * mean_ms / 1000.0).min(3600.0))
}

/// What the room service tells a visitor past its cap.
pub const FULL: &str = "the room is full";
/// What it tells a spectator past the spectator cap.
pub const WATCHING_FULL: &str = "too many people are watching";

/// Guests' measurements for `/join`, spectators' for `/spectate`.
fn stats_for<'a>(shared: &'a Shared, path: &str) -> &'a Stats {
    if path == "/spectate" {
        &shared.spectator_stats
    } else {
        &shared.stats
    }
}

pub fn now_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Ask the service at `path` for pre-admissions, then redeem each with a new
/// person's one-time token, as a guest's page does. Past a cap, the error is
/// the service's own message ([`FULL`] or [`WATCHING_FULL`]).
pub async fn admit(
    shared: &Shared,
    path: &str,
    request: serde_json::Value,
    name: &str,
) -> Result<(SigningKey, DeviceCert, Vec<JoinedRoom>), String> {
    let asked = Instant::now();
    let (status, body) = shared
        .service
        .post_with(
            path,
            &[("content-type", "application/json")],
            request.to_string().into_bytes(),
        )
        .await?;
    if status == 503
        && let Some(full) = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|reply| {
                reply
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|error| error == FULL || error == WATCHING_FULL)
    {
        return Err(full);
    }
    if status != 200 {
        return Err(format!("{path}: status {status}"));
    }
    stats_for(shared, path).service(asked.elapsed().as_secs_f64() * 1000.0);
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
    let request = serde_json::json!({ "userPublicKey": "", "displayName": name });
    let (key, cert, rooms) = match admit(&shared, "/join", request, &name).await {
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
            watching: false,
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
        record: CHAT_RECORD.into(),
    });

    // The cursor: a fallback live session on the desktop room.
    let mut live = None;
    if shared.args.cursor_hz > 0.0
        && let Some(desktop) = desktop
    {
        live = open_live(&peer, desktop, &shared.relay, &stats).await;
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
            let message = chat_message(now_ms(shared.started), shared.args.chat_bytes);
            if peer
                .publish(route.clone(), &DocumentMutation::Incremental(message))
                .is_ok()
            {
                stats.chat_tx.fetch_add(1, Ordering::Relaxed);
            }
            next_chat = Some(now + exponential(shared.args.chat_every * 1000.0));
        }
        seen_failures = take_events(&peer, &stats, shared.started, joined_at, seen_failures);
    }
    peer.close();
}

/// Open the live plane of `workspace` with the session's live grant, counting
/// what its stream delivers into `stats`.
pub async fn open_live(
    peer: &PeerHandle,
    workspace: u64,
    relay: &Arc<Plain>,
    stats: &Arc<Stats>,
) -> Option<Live> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let asked = peer.live_grant_then(
        workspace,
        Box::new(move |grant| {
            let _ = sender.send(grant);
        }),
    );
    match (asked, receiver.await) {
        (Ok(()), Ok(Ok(grant))) => {
            match Live::open(relay.clone(), &grant.token, stats.clone()).await {
                Ok(opened) => {
                    stats.live_open.fetch_add(1, Ordering::Relaxed);
                    Some(opened)
                }
                Err(error) => {
                    stats.fail(error);
                    None
                }
            }
        }
        (Err(error), _) | (_, Ok(Err(error))) => {
            stats.fail(format!("live grant: {error}"));
            None
        }
        (_, Err(_)) => {
            stats.fail("live grant: dropped");
            None
        }
    }
}

/// Count what the session delivered since the last call: chat changes with
/// their delivery time (or as history, when sent before `joined_at`),
/// commits, rejections, and session failures beyond the `seen` first ones.
/// Returns the failures seen so far.
pub fn take_events(
    peer: &PeerHandle,
    stats: &Stats,
    started: Instant,
    joined_at: u64,
    seen: usize,
) -> usize {
    for event in peer.drain() {
        match event {
            PeerEvent::Document(document) => match document.mutation() {
                Some(DocumentMutation::Incremental(bytes)) if chat_sent_ms(&bytes).is_some() => {
                    let sent = chat_sent_ms(&bytes).unwrap_or_default();
                    if sent < joined_at {
                        stats.chat_history();
                    } else {
                        let latency = now_ms(started).saturating_sub(sent);
                        stats.chat(sent, latency as f64);
                    }
                }
                // The governor's compaction of the chat's history: messages
                // the page already holds, as one snapshot.
                Some(DocumentMutation::Snapshot(_)) => stats.chat_history(),
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
    let session = peer.status();
    let failures = session.failures.len();
    if failures > seen {
        stats
            .session_failures
            .fetch_add((failures - seen) as u64, Ordering::Relaxed);
        for failure in session.failures.iter().skip(seen) {
            stats.fail(format!("session: {} {}", failure.code, failure.message));
        }
    }
    failures
}
