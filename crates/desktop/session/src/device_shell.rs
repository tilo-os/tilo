//! The desktop as the shell of the device's daemon: how its person's workspaces stand, read off
//! the daemon, and the actions that change them, asked of it
//! ([`tilo_session_client::workspace_view`]).
//!
//! The session's one shell connection is the link the daemon handed it as it started it, kept for
//! as long as the session runs: a link is used once, and no other way in is the shell. It watches the
//! person's workspace list and each listed workspace's standing, keeps the last reading of each
//! in the [`WorkspaceView`] the loop reads, and wakes the loop when one changed. Every change to
//! a workspace (founding one, joining one, a governance action, the host's own operations) is an
//! action the daemon answers on that connection.

use std::collections::BTreeSet;
use std::sync::Arc;

use runtime_collab::ShellClient;
use runtime_collab::wire::shell::{ShellCall, ShellReply};
use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_view::{LIST, Listed, resource};
use tilo_session_client::engine::ServiceAnswer;
use tilo_session_client::workspace_view::{ShellAsks, WorkspaceView};

/// The daemon, as this session reaches it: on the session's one shell connection.
struct Daemon {
    client: ShellClient,
}

impl Daemon {
    /// Invoke `request` on the daemon, answered with its output once it settled.
    async fn invoke(self: Arc<Self>, request: InvokeRequest) -> Result<serde_json::Value, String> {
        let request = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        let reply = self
            .client
            .call(ShellCall::Invoke { request })
            .await
            .map_err(|error| error.to_string())?;
        let ShellReply::Invoked(update) = reply else {
            return Err("this device's daemon answered an action with something else".to_owned());
        };
        serde_json::from_str(&update?)
            .map_err(|error| format!("this device's daemon's answer does not read: {error}"))
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
        self.invoke(tilo_session_client::workspace_view::request(
            target, arguments,
        ))
    }

    fn invoke(&self, request: InvokeRequest) -> ServiceAnswer {
        Box::pin(Arc::clone(&self.0).invoke(request))
    }
}

/// The view of the person's workspaces on this device, as the daemon this session's shell
/// connection `client` reaches publishes it: followed from now on, `wake` called each time a reading changed it. `None`
/// when this computer has no identity to be a person's shell with.
pub(crate) fn open(
    client: ShellClient,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Option<Arc<WorkspaceView>> {
    let identity = tilo_session_client::identity::device_identity()?;
    let daemon = Arc::new(Daemon { client });
    let view = Arc::new(WorkspaceView::new(
        identity.device_cert.user_public_key,
        identity.device_cert.device_public_key,
        Arc::new(Asks(Arc::clone(&daemon))) as Arc<dyn ShellAsks>,
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
        if let Err(error) = follow(&daemon, &following, &wake).await {
            tracing::warn!(%error, "the session stopped following its workspaces");
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
    loop {
        let reading = list.read().await.map_err(|error| error.to_string())?;
        if view.apply(LIST, reading.reading.as_deref())? {
            wake();
        }
        let listed: Vec<Listed> = view.listed();
        for workspace in listed.into_iter().filter(|listed| listed.kept_here) {
            if !followed.lock_recover().insert(workspace.id.clone()) {
                continue;
            }
            let (standing, view, wake, followed) = (
                standing.clone(),
                Arc::clone(view),
                Arc::clone(wake),
                Arc::clone(&followed),
            );
            tokio::spawn(async move {
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
    loop {
        let reading = readings.read().await.map_err(|error| error.to_string())?;
        let ended = reading.reading.is_none();
        if view.apply(named, reading.reading.as_deref())? {
            wake();
        }
        if ended {
            return Ok(());
        }
    }
}
