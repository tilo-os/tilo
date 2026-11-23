//! The tokens a host's fetch proxy takes: a page asks for one in the workspace
//! it is in ([`tilo_server_protocol::workspaces::FETCH_GRANT`]) and sends it
//! with each request it makes through the proxy
//! ([`tilo_server_protocol::web::FETCH_PATH`]), which admits the person and
//! device each names. A host that serves no proxy mints none.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The tokens a host's fetch proxy takes.
#[derive(Default)]
pub struct Grants {
    /// Whether a proxy is served here, which a listener says as it starts.
    serving: AtomicBool,
    /// The person and device each token names.
    open: Mutex<HashMap<[u8; 32], Granted>>,
}

/// The person and device a token was minted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Granted {
    /// The person the proxy's requests count for.
    pub person: [u8; 32],
    /// The device they are made for.
    pub device: [u8; 32],
}

impl Grants {
    /// This host serves a proxy from now on.
    pub fn serves(&self) {
        self.serving.store(true, Ordering::Relaxed);
    }

    /// Whether this host serves a proxy.
    #[must_use]
    pub fn served(&self) -> bool {
        self.serving.load(Ordering::Relaxed)
    }

    /// A token naming `device` of `person`.
    #[must_use]
    pub fn grant(&self, person: [u8; 32], device: [u8; 32]) -> Option<[u8; 32]> {
        let token: [u8; 32] = rand::random();
        self.open
            .lock()
            .ok()?
            .insert(token, Granted { person, device });
        Some(token)
    }

    /// What `token` names, while it is open.
    #[must_use]
    pub fn granted(&self, token: &[u8; 32]) -> Option<Granted> {
        self.open.lock().ok()?.get(token).copied()
    }
}
