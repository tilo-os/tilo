//! One watching page, holding a session subscribed to the first desktop
//! room and reading its documents and its live plane without ever sending.
//!
//! By default it gets in as the homepage embed does: it certifies an
//! in-memory device key as the room's one spectator person through
//! `POST /spectate`. With `--spectator-members`, each spectator is instead
//! pre-admitted as a Viewer of its own (`/spectate` with `"member": true`)
//! and redeems that as a guest's page does: what a read-only session costs a
//! room whose spectators are admitted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde::Deserialize;
use tilo_browser_peer::{Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::guest::{FULL, Shared, WATCHING_FULL, admit, now_ms, open_live, take_events};

/// How often a watching page looks at what arrived.
const TICK: Duration = Duration::from_millis(100);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spectating {
    certificate: DeviceCert,
    workspace_id: u64,
}

/// The device key, its certificate and the room to watch.
async fn certify(shared: &Shared) -> Result<(SigningKey, DeviceCert, u64), String> {
    if shared.args.spectator_members {
        let request = serde_json::json!({ "member": true });
        let (key, cert, rooms) = admit(shared, "/spectate", request, "Spectator").await?;
        let workspace = rooms.first().ok_or("no room to watch")?.workspace_id;
        return Ok((key, cert, workspace));
    }
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
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
    let spectating: Spectating =
        serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    Ok((key, spectating.certificate, spectating.workspace_id))
}

/// Run one spectator until the run stops.
pub async fn run(shared: Arc<Shared>) {
    let stats = shared.spectator_stats.clone();
    let begun = Instant::now();
    let (key, cert, workspace) = match certify(&shared).await {
        Ok(certified) => certified,
        Err(error) if error == WATCHING_FULL || error == FULL => {
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
            cert,
            host_cert: shared.host_cert.clone(),
            revocations: RevocationList::new(),
            watching: true,
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    );
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
            // Name what the host answered: a refused subscription is a
            // finding, not a slow one.
            let session = peer.status();
            stats.fail(format!(
                "join: no Ready before the timeout (authenticated: {}, failures: {:?})",
                session.authenticated,
                session
                    .failures
                    .iter()
                    .map(|failure| format!("{} {}", failure.code, failure.message))
                    .collect::<Vec<_>>()
            ));
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
        seen_failures = take_events(
            &peer,
            &stats,
            (shared.started, joined_at),
            seen_failures,
            &mut None,
        );
    }
    peer.close();
}
