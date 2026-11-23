//! Tilo's API (`docs/developers/api.md`) for the person sitting here, at
//! `$XDG_RUNTIME_DIR/tilo-session/api.sock`: what their own programs on this computer (the REPL,
//! scripts, the SDKs) reach this desktop with, as them. It is the API server's own code
//! ([`tilo_api::direct::Direct::here`]) acting as this device's key, and this session answers each
//! workspace itself, through the broker's ordinary permission checks, as the person. Only this account's programs
//! are served: the socket sits in the login's private directory, and a connection from another
//! account is refused.
//!
//! Agents never come here: they reach the desktop at its MCP server ([`crate::mcp_server`]),
//! where the person admits each one.

use std::path::Path;
use std::sync::Arc;

use tilo_workspace_service::apps::collab_app_broker::AppBrokerState;

use crate::mcp_server::{Host as SessionApi, SessionServices, person_tasks};

/// This session, as the API server inside it reaches the person's workspaces.
struct Desktop {
    broker: Arc<AppBrokerState>,
    services: Arc<SessionServices>,
}

impl Desktop {
    /// Whether `key` is this desktop's person's: the only keys its API serves, on a socket only
    /// their own programs reach.
    fn persons(&self, key: &tilo_user_identity::DeviceCert) -> bool {
        hex::encode(key.user_public_key) == self.services.person()
    }
}

impl tilo_api::session::Here for Desktop {
    fn workspaces(
        &self,
        key: &tilo_user_identity::DeviceCert,
    ) -> Vec<tilo_api::session::Workspace> {
        if !self.persons(key) {
            return Vec::new();
        }
        self.services
            .workspaces()
            .into_iter()
            .map(|[id, name]| tilo_api::session::held(id, name))
            .collect()
    }

    fn enter(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> Result<tilo_api::session::Reached, String> {
        if !self.persons(key.cert())
            || !self
                .services
                .workspaces()
                .iter()
                .any(|[id, _]| id == workspace)
        {
            return Err("no such workspace".into());
        }
        // The socket is this login's alone, as the owner's socket is: what comes through it is
        // the person at this device, and the broker keeps the device's certificate as the owner
        // of each step, as it does for the person's clicks.
        let api = SessionApi::new(Some(Arc::clone(&self.broker)), Arc::clone(&self.services))
            .with_session_actions(self.services.actions())
            .for_local_person();
        Ok(tilo_api::session::Reached {
            provider: Arc::new(api),
            person: self.services.person().to_owned(),
            caller: None,
            tasks: person_tasks(&self.services).map(|tasks| tasks as Arc<dyn tilo_mcp::TaskHosts>),
        })
    }
}

/// Serve the API at `path`, over `broker`, as this device's key with its certificate. Nothing is
/// served, saying why, when the session has no broker or this device holds no key of its own.
pub(crate) fn serve(
    path: &Path,
    broker: Option<Arc<AppBrokerState>>,
    services: Arc<SessionServices>,
) {
    let Some(broker) = broker else {
        tracing::warn!("no API for this computer's programs: the session serves no broker");
        return;
    };
    let device = crate::sync::device_identity().and_then(|identity| {
        let secret = identity.signer.endpoint_secret()?;
        Some((
            ed25519_dalek::SigningKey::from_bytes(&secret),
            identity.device_cert,
        ))
    });
    let Some((key, cert)) = device else {
        tracing::warn!("no API for this computer's programs: this device holds no key of its own");
        return;
    };
    let name = cert.device_label.clone();
    let direct = match tilo_api::direct::Direct::here(
        Arc::new(Desktop { broker, services }),
        key,
        cert,
        &name,
    ) {
        Ok(direct) => direct,
        Err(error) => {
            tracing::warn!(%error, "no API for this computer's programs");
            return;
        }
    };
    let listener = match crate::mcp_server::bind_private(path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "no API for this computer's programs");
            return;
        }
    };
    // Each request is answered on a runtime of the server's own, as the MCP server answers each
    // connection on a thread of its own: the session's actions wait on the app runtime, which
    // must never be the thread that waits.
    let serving = std::thread::Builder::new()
        .name("tilo-api".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("tilo-api")
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(%error, "no API for this computer's programs");
                    return;
                }
            };
            runtime.block_on(async move {
                match listener
                    .set_nonblocking(true)
                    .and_then(|()| tokio::net::UnixListener::from_std(listener))
                {
                    Ok(listener) => direct.serve(listener).await,
                    Err(error) => tracing::warn!(%error, "no API for this computer's programs"),
                }
            });
        });
    match serving {
        Ok(_) => {
            tracing::info!(path = %path.display(), "serving Tilo's API to this computer's programs");
        }
        Err(error) => tracing::warn!(%error, "no API for this computer's programs"),
    }
}
