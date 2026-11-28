//! The desktop as the shell of the device's daemon: how its person's workspaces stand, read off
//! the daemon, and the actions that change them, asked of it
//! ([`tilo_session_client::workspace_view`]).
//!
//! The session's one shell connection is the link the daemon handed it as it started it, kept for
//! as long as the session runs: a link is used once, and no other way in is the shell. It watches the
//! person's workspace list and each listed workspace's standing, keeps the last reading of each
//! in the [`WorkspaceView`] the loop reads, and wakes the loop when one changed. Every change to
//! a workspace (founding one, joining one, a governance action, the host's own operations) is an
//! action the daemon's one dispatch answers on that connection, as the person.

use std::collections::BTreeSet;
use std::sync::Arc;

use runtime_collab::ShellClient;
use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_view::{LIST, Listed, Part, part as part_resource, resource};
use tilo_session_client::engine::ServiceAnswer;
use tilo_session_client::workspace_view::{ShellAsks, WorkspaceView};

/// The daemon, as this session reaches it: on the session's one shell connection.
struct Daemon {
    client: ShellClient,
}

impl Daemon {
    /// Invoke `request` through the daemon's dispatch, as the person, answered with its output once
    /// it settled.
    async fn invoke(
        self: Arc<Self>,
        workspace: Option<String>,
        request: InvokeRequest,
    ) -> Result<serde_json::Value, String> {
        self.client.dispatch().settled(workspace, request).await
    }
}

/// The daemon's actions, asked on the session's one shell connection.
struct Asks(Arc<Daemon>);

impl ShellAsks for Asks {
    fn account(
        &self,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> ServiceAnswer {
        let target = RemoteActionTarget {
            provider: tilo_action_policy::agent_answers::ACCOUNT_APP.to_owned(),
            address: tilo_actions::ActionAddress::new(resource.to_owned(), action),
        };
        self.invoke(
            None,
            tilo_session_client::workspace_view::request(target, arguments),
        )
    }

    fn invoke(&self, workspace: Option<String>, request: InvokeRequest) -> ServiceAnswer {
        Box::pin(Arc::clone(&self.0).invoke(workspace, request))
    }
}

/// The view of the person's workspaces on this device, as the daemon this session's shell
/// connection `client` reaches publishes it: followed from now on, `wake` called each time a reading changed it. `None`
/// when this computer has no identity to be a person's shell with.
/// What a view asks the daemon on `client`: its actions, carried out by the daemon's dispatch as
/// the person.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn asks(client: ShellClient) -> Arc<dyn ShellAsks> {
    asks_of(&Arc::new(Daemon { client }))
}

fn asks_of(daemon: &Arc<Daemon>) -> Arc<dyn ShellAsks> {
    Arc::new(Asks(Arc::clone(daemon)))
}

pub(crate) fn open(
    client: ShellClient,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Option<Arc<WorkspaceView>> {
    let identity = tilo_session_client::identity::device_identity()?;
    let daemon = Arc::new(Daemon { client });
    let view = Arc::new(WorkspaceView::new(
        identity.device_cert.user_public_key,
        identity.device_cert.device_public_key,
        asks_of(&daemon),
    ));
    let runtime = match tilo_rt::try_app() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "the session follows none of its workspaces");
            return Some(view);
        }
    };
    let following = Arc::clone(&view);
    runtime.spawn(async move {
        // Each link's subscriptions end with it, and the old follower has let go of every
        // workspace before the next reads them again: a daemon that restarted is followed as the
        // one before it was.
        let mut linking = daemon.client.linking();
        loop {
            if let Err(error) = follow(&daemon, &following, &wake).await {
                tracing::warn!(%error, "the session follows its workspaces no more on this link");
            }
            if matches!(
                daemon.client.link_status(),
                runtime_collab::ShellLink::Linked
            ) {
                // A follower that ended before the link did asks again shortly.
                tokio::select! {
                    moved = linking.changed() => {
                        if moved.is_err() {
                            return;
                        }
                    }
                    () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                }
            } else if linking.changed().await.is_err() {
                return;
            }
        }
    });
    Some(view)
}

/// Follow the person's workspace list into `view`, and the standing of each workspace it lists
/// this device as keeping, for as long as the daemon publishes them.
async fn follow(
    daemon: &Daemon,
    view: &Arc<WorkspaceView>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), String> {
    let standing = daemon.client.standing();
    let list = standing
        .watch(LIST)
        .await
        .map_err(|error| error.to_string())?;
    let followed: Arc<std::sync::Mutex<BTreeSet<String>>> = Arc::default();
    // Each workspace's follow lives as long as this one: when the link ends, they are let go
    // before the next link's follower reads anything, so none clears a reading the next applied.
    let mut following = tokio::task::JoinSet::new();
    loop {
        // A follow whose workspace left the list is done with; one that failed says why.
        while let Some(joined) = following.try_join_next() {
            if let Err(error) = joined {
                tracing::warn!(%error, "a workspace's follow stopped");
            }
        }
        let reading = list.read().await.map_err(|error| error.to_string())?;
        if view.apply(LIST, reading.reading.as_deref())? {
            wake();
        }
        // Every workspace the daemon lists is one it holds: each one's standing is followed, whether
        // this device runs its work or another does.
        let listed: Vec<Listed> = view.listed();
        for workspace in listed {
            if !followed.lock_recover().insert(workspace.id.clone()) {
                continue;
            }
            let (standing, view, wake, followed) = (
                standing.clone(),
                Arc::clone(view),
                Arc::clone(wake),
                Arc::clone(&followed),
            );
            // What the daemon publishes beside the standing is followed as long as it does.
            for part in Part::ALL {
                let (standing, view, wake) =
                    (standing.clone(), Arc::clone(&view), Arc::clone(&wake));
                let named = part_resource(&workspace.id, part);
                following.spawn(async move {
                    if let Err(error) = follow_one(&standing, &named, &view, &wake).await {
                        tracing::warn!(%error, named, "the session stopped following a workspace's part");
                    }
                    if view.apply(&named, None) == Ok(true) {
                        wake();
                    }
                });
            }
            following.spawn(async move {
                let named = resource(&workspace.id);
                if let Err(error) = follow_one(&standing, &named, &view, &wake).await {
                    tracing::warn!(%error, workspace = workspace.id, "the session stopped following a workspace");
                }
                // A workspace listed again is followed again.
                if view.apply(&named, None) == Ok(true) {
                    wake();
                }
                followed.lock_recover().remove(&workspace.id);
            });
        }
    }
}

/// Follow `named`'s standing into `view` until the daemon publishes it no more.
async fn follow_one(
    standing: &runtime_collab::ServicesHandle,
    named: &str,
    view: &WorkspaceView,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), String> {
    let readings = standing
        .watch(named)
        .await
        .map_err(|error| error.to_string())?;
    // A resource found before it is published reads as no reading yet, which is not the same as
    // one that ended: a workspace listed just as its standing is being published, or founded by
    // this session, must keep its follower waiting for the value rather than leave it empty.
    let mut held = false;
    loop {
        let reading = readings.read().await.map_err(|error| error.to_string())?;
        let ended = reading.reading.is_none() && held;
        if reading.reading.is_some() {
            held = true;
        }
        if view.apply(named, reading.reading.as_deref())? {
            wake();
        }
        if ended {
            return Ok(());
        }
    }
}
