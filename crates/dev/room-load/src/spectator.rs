//! One watching page, as the homepage embed runs it: certify an in-memory
//! device key as the room's spectator person through `POST /spectate`, hold
//! a session subscribed to the first desktop room, and read its documents
//! and its live plane without ever sending.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde::Deserialize;
use tilo_browser_peer::{Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::guest::{Shared, now_ms, open_live, take_events};

/// What the room service tells a spectator past the spectator cap.
const WATCHING_FULL: &str = "too many people are watching";
/// How often a watching page looks at what arrived.
const TICK: Duration = Duration::from_millis(100);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spectating {
    certificate: DeviceCert,
    workspace_id: u64,
}

async fn certify(shared: &Shared, key: &SigningKey) -> Result<Spectating, String> {
    let (status, body) = shared
        .service
        .post_with(
            "/spectate",
            &[("content-type", "application/json")],
            serde_json::json!({ "devicePublicKey": hex::encode(key.verifying_key().to_bytes()) })
                .to_string()
                .into_bytes(),
        )
        .await?;
    if status == 503
        && serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|reply| {
            reply.get("error").and_then(serde_json::Value::as_str) == Some(WATCHING_FULL)
        })
    {
        return Err(WATCHING_FULL.into());
    }
    if status != 200 {
        return Err(format!("/spectate: status {status}"));
    }
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

/// Run one spectator until the run stops.
pub async fn run(shared: Arc<Shared>) {
    let stats = shared.spectator_stats.clone();
    let begun = Instant::now();
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let spectating = match certify(&shared, &key).await {
        Ok(spectating) => spectating,
        Err(error) if error == WATCHING_FULL => {
            stats.full(begun.elapsed().as_secs_f64() * 1000.0);
            return;
        }
        Err(error) => {
            stats.fail(format!("spectate: {error}"));
            return;
        }
    };
    let admitted_ms = begun.elapsed().as_secs_f64() * 1000.0;
    let peer = PeerHandle::start(
        PeerConfig {
            host: Host::Https(shared.relay.clone()),
            cert: spectating.certificate,
            host_cert: shared.host_cert.clone(),
            revocations: RevocationList::new(),
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    );
    let workspace = spectating.workspace_id;
    if let Err(error) = peer.join(workspace) {
        stats.fail(format!("subscribe: {error}"));
        return;
    }
    let mut stop = shared.stop.clone();
    let deadline = begun + Duration::from_secs(shared.args.join_timeout);
    let mut ready = false;
    while !ready {
        ready = peer.drain().into_iter().any(
            |event| matches!(event, PeerEvent::Ready { workspace_id } if workspace_id == workspace),
        );
        if !ready && (Instant::now() > deadline || *stop.borrow()) {
            stats.fail("join: no Ready before the timeout");
            peer.close();
            return;
        }
        if !ready {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    stats.join(admitted_ms, begun.elapsed().as_secs_f64() * 1000.0);
    let joined_at = now_ms(shared.started);
    // The embed draws the room's cursors: it reads the live plane too.
    let _live = if shared.args.cursor_hz > 0.0 {
        open_live(&peer, workspace, &shared.relay, &stats).await
    } else {
        None
    };
    let mut interval = tokio::time::interval(TICK);
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
        seen_failures = take_events(&peer, &stats, shared.started, joined_at, seen_failures);
    }
    peer.close();
}
