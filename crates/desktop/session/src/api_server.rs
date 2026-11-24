//! Tilo's API (`docs/developers/api.md`) for the person sitting here, at
//! `$XDG_RUNTIME_DIR/tilo-session/api.sock`: what their own programs on this computer (the REPL,
//! scripts, the SDKs) reach this desktop with, as them. It is the API server's own code
//! ([`tilo_api::direct::Direct::here`]) acting as this device's key, and this session answers each
//! workspace itself, through the device's one dispatch, as the person. Only this account's programs
//! are served: the socket sits in the login's private directory, and a connection from another
//! account is refused.
//!
//! Agents never come here: they reach the desktop at its MCP server ([`crate::mcp_server`]),
//! where the person admits each one.

use std::path::Path;
use std::sync::Arc;

use crate::mcp_server::{Dispatch, Host as SessionApi, SessionServices, person_tasks};

/// This session, as the API server inside it reaches the person's workspaces.
struct Desktop {
    dispatch: Arc<Dispatch>,
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
        let api = SessionApi::new(Some(Arc::clone(&self.dispatch)), Arc::clone(&self.services))
            .with_session_actions(self.services.actions());
        Ok(tilo_api::session::Reached {
            provider: Arc::new(api),
            person: self.services.person().to_owned(),
            caller: None,
            tasks: person_tasks(&self.services).map(|tasks| tasks as Arc<dyn tilo_mcp::TaskHosts>),
        })
    }
}

/// Serve the API at `path`, over `dispatch`, the device's own as the person, as this device's key
/// with its certificate. Nothing is served, saying why, when the session reaches no daemon or this
/// device holds no key of its own.
pub(crate) fn serve(
    path: &Path,
    dispatch: Option<Arc<Dispatch>>,
    services: Arc<SessionServices>,
) {
    let Some(dispatch) = dispatch else {
        tracing::warn!("no API for this computer's programs: the session reaches no daemon");
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
        Arc::new(Desktop { dispatch, services }),
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
