//! The apps' long-lived connections this device keeps ([`Turn::Connection`]): one turn per app
//! that keeps one and workspace this device keeps, taken where [`takes_turn_in`] says, renewed
//! while the app's process runs, and given up the moment this device may no longer be the one
//! that runs it.

use std::collections::BTreeMap;
use std::time::Duration;

use tilo_workspace_service::work::{
    Claim, HostConnections, NotTaken, Order, StartRefusal, Turn, eligible, takes_turn_in,
};
use tilo_workspace_service::workspaces::Workspaces;

/// How long a connection's claim goes between renewals.
const RENEW: Duration = Duration::from_secs(15);

/// How long this device may go without renewing a connection's claim before it stops the
/// connection, whatever its own state says: well inside the minute another device waits for a
/// silent claim ([`tilo_peer_state::CLOCK_SKEW_NS`]), so a device that slept or stalled stops
/// before the device that takes over starts, and asks for the turn again.
const LAPSED: Duration = Duration::from_secs(40);

/// Which connection: the workspace and the app.
type Key = (String, String);

/// A connection this device holds the lease of, with its process running.
struct Holding {
    turn: Turn,
    claim: Claim,
    renewed_ms: i64,
}

/// The connections this device holds, and what it saw of the others.
#[derive(Default)]
pub(super) struct Connections {
    holding: BTreeMap<Key, Holding>,
    /// When this device first saw each connection it does not hold free, ms since the epoch.
    free_since: BTreeMap<Key, i64>,
    /// When this device asks again for a connection whose process failed to start here.
    again_at_ms: BTreeMap<Key, i64>,
    /// The reason each connection last failed with, so the same one is said in the log once
    /// rather than on every look that waits it out.
    reported: BTreeMap<Key, String>,
}

impl Connections {
    /// Look at every connection once, at `now_ms`: keep the ones held here, give up the ones this
    /// device may no longer run, and take the ones whose turn is this device's.
    pub(super) async fn tend(&mut self, workspaces: &Workspaces, order: Order, now_ms: i64) {
        let Some(host) = workspaces.host_connections() else {
            return;
        };
        let mut wanted = Vec::new();
        for workspace in workspaces.described().await {
            let workspace = workspace.id().to_string();
            for (app, needs) in host.connections() {
                wanted.push(((workspace.clone(), app), needs));
            }
        }
        let held: Vec<Key> = self.holding.keys().cloned().collect();
        for key in held {
            if !wanted.iter().any(|(wanted, _)| *wanted == key) {
                tracing::info!(workspace = %key.0, app = %key.1, "an app's connection is no longer kept here");
                self.give_up(host.as_ref(), &key, true).await;
            }
        }
        for (key, needs) in wanted {
            if self.holding.contains_key(&key) {
                self.keep(workspaces, host.as_ref(), &key, now_ms).await;
            } else {
                self.take(workspaces, host.as_ref(), (key, needs), order, now_ms)
                    .await;
            }
        }
    }

    /// Keep the connection `key` held here: stop it when its claim lapsed or could not be
    /// renewed, or this device no longer has what the app needs, renew it when it is due, and
    /// start its process again when it ended.
    async fn keep(
        &mut self,
        workspaces: &Workspaces,
        host: &dyn HostConnections,
        key: &Key,
        now_ms: i64,
    ) {
        let Some(holding) = self.holding.get_mut(key) else {
            return;
        };
        let since = now_ms.saturating_sub(holding.renewed_ms);
        if since >= millis(LAPSED) {
            tracing::warn!(workspace = %key.0, app = %key.1, since_ms = since, "an app's connection went unrenewed past its claim, so this device stops it");
            self.give_up(host, key, false).await;
            return;
        }
        if since >= millis(RENEW) {
            if let Err(why) = eligible(workspaces, &key.0, &holding.turn).await {
                tracing::info!(workspace = %key.0, app = %key.1, %why, "this device lets another take an app's connection");
                self.give_up(host, key, true).await;
                return;
            }
            if !holding.claim.renew().await {
                tracing::info!(workspace = %key.0, app = %key.1, "this device can no longer hold an app's connection, so it stops it");
                self.give_up(host, key, false).await;
                return;
            }
            holding.renewed_ms = now_ms;
        }
        if !host.runs(&key.0, &key.1) {
            tracing::warn!(workspace = %key.0, app = %key.1, "an app's connection ended, so this device starts it again");
            match host.start(&key.0, &key.1).await {
                Ok(()) => {
                    self.again_at_ms.remove(key);
                    self.reported.remove(key);
                }
                Err(refusal) => {
                    self.refused(key, &refusal, now_ms);
                    self.give_up(host, key, true).await;
                }
            }
        }
    }

    /// Ask for the connection `key`'s turn, and start its process when this device takes it.
    async fn take(
        &mut self,
        workspaces: &Workspaces,
        host: &dyn HostConnections,
        (key, needs): (Key, Vec<String>),
        order: Order,
        now_ms: i64,
    ) {
        if self
            .again_at_ms
            .get(&key)
            .is_some_and(|again| now_ms < *again)
        {
            return;
        }
        self.again_at_ms.remove(&key);
        let free_since = *self.free_since.entry(key.clone()).or_insert(now_ms);
        let turn = Turn::connection(key.1.clone(), needs).free_since(free_since);
        match takes_turn_in(workspaces, &key.0, &turn, order).await {
            Ok(claim) => {
                self.free_since.remove(&key);
                if let Err(refusal) = host.start(&key.0, &key.1).await {
                    self.refused(&key, &refusal, now_ms);
                    claim.release().await;
                    return;
                }
                self.again_at_ms.remove(&key);
                self.reported.remove(&key);
                tracing::info!(workspace = %key.0, app = %key.1, "this device holds an app's connection");
                self.holding.insert(
                    key,
                    Holding {
                        turn,
                        claim,
                        renewed_ms: now_ms,
                    },
                );
            }
            Err(NotTaken::Waiting { .. }) => {}
            Err(NotTaken::TakenBy(_) | NotTaken::Ineligible(_) | NotTaken::Done(_)) => {
                self.free_since.remove(&key);
                // A process left from before this device lost the lease never runs beside the
                // device that holds it.
                if host.runs(&key.0, &key.1) {
                    tracing::warn!(workspace = %key.0, app = %key.1, "an app's connection ran here while this device did not hold it, so it stops");
                    host.stop(&key.0, &key.1);
                }
            }
            Err(NotTaken::Unwritten(error)) => {
                tracing::warn!(workspace = %key.0, app = %key.1, %error, "this device could not claim an app's connection");
            }
        }
    }

    /// Wait out `refusal` before asking for `key` again, and say its reason the first time it is
    /// seen: a start that keeps failing the same way is not restarted in a loop, and the reason
    /// is not repeated in the log by every look that waits.
    fn refused(&mut self, key: &Key, refusal: &StartRefusal, now_ms: i64) {
        if self.reported.get(key) != Some(&refusal.reason) {
            tracing::error!(
                workspace = %key.0,
                app = %key.1,
                reason = %refusal.reason,
                "an app's connection did not come up"
            );
            self.reported.insert(key.clone(), refusal.reason.clone());
        }
        self.again_at_ms
            .insert(key.clone(), now_ms.saturating_add(millis(refusal.again)));
    }

    /// Stop the connection `key` held here, and let go of its claim when `release`: a claim
    /// that lapsed or was lost is left to go silent.
    async fn give_up(&mut self, host: &dyn HostConnections, key: &Key, release: bool) {
        host.stop(&key.0, &key.1);
        if let Some(holding) = self.holding.remove(key)
            && release
        {
            holding.claim.release().await;
        }
    }

    /// The connections this device holds now, each by its workspace and app.
    pub(super) fn held(&self) -> Vec<(String, String)> {
        self.holding.keys().cloned().collect()
    }
}

/// `duration` in whole milliseconds.
fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
